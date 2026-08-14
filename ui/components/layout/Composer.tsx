import {
  useRef,
  useState,
  useEffect,
  useCallback,
  useMemo,
  KeyboardEvent,
  DragEvent,
  FormEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { useStore, TODO_DRAG_MIME, splitLeafTabIds } from "../../state/store";
import { pathsFromDataTransfer } from "../../state/dropPaths";
import { sendPromptToAgent } from "../../state/agentActions";
import { createDefaultAgent } from "../../state/structuredAgent";
import { ContextWindowMeter } from "./ContextWindowMeter";
import { Icon } from "../Icon";

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

interface SlashItem {
  name: string;
  description: string;
  /** Exact provider-native text inserted into the Composer. */
  invocation: string;
  source: string;
  kind: "skill" | "command";
  /** Backends mark commands whose options open a second-level picker. Older
      responses omit the field — treat missing as falsy. */
  has_children?: boolean;
}

/** One level of the drill-down `/` menu stack. The deepest level is the one
    being navigated; ancestors exist only so Esc can roll back level by level. */
interface SlashMenuState {
  /** Textarea index where this level's filter input begins. Root = the `/`
      index; child levels = right after the parent invocation + space. */
  anchor: number;
  /** Text length to truncate to on Esc, removing the parent invocation that
      opened this level. Root level never truncates (kept at -1). */
  rollbackTo: number;
  /** The selected parent item that opened this level (`null` for root). */
  parent: SlashItem | null;
  /** Full, unfiltered items; filtered by `query` at render/navigation time. */
  items: SlashItem[];
  idx: number;
  loading: boolean;
  /** Filter text typed within this level (e.g. `mod` under `/model`). */
  query: string;
}

interface ComposerDraftState {
  value: string;
  selectionStart: number;
  selectionEnd: number;
}

/** Where a Composer send goes. Tab cycles through the open terminals (live
 *  agent sessions) + the 待分配 todo area; a null cycle target means "follow the
 *  active tab" (the pre-existing behavior). */
type ComposerTarget =
  | { kind: "agent"; agentId: string }
  | { kind: "todo" };

export function Composer() {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const atMenuRef = useRef<HTMLDivElement>(null);
  const slashMenuRef = useRef<HTMLDivElement>(null);

  const composerOpen = useStore((s) => s.composerOpen);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const splitTree = useStore((s) => s.splitTree);
  const agents = useStore((s) => s.agents);
  const addTodo = useStore((s) => s.addTodo);

  const toggleComposer = useStore((s) => s.toggleComposer);
  const composerH = useStore((s) => s.composerH);
  const setComposerH = useStore((s) => s.setComposerH);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);

  const [atMenu, setAtMenu] = useState<AtMenuState | null>(null);
  const [slashMenuStack, setSlashMenuStack] = useState<SlashMenuState[]>([]);
  // Tab-cycled send target: null follows the active tab (pre-existing behavior).
  const [cycleTarget, setCycleTarget] = useState<ComposerTarget | null>(null);
  const [dragHover, setDragHover] = useState(false);
  const [isBangInput, setIsBangInput] = useState(false);
  // Non-empty input → enables the send button (`.ul-send-btn`).
  const [hasInput, setHasInput] = useState(false);
  // Root ref + dragging flag for the height-resize divider above the composer.
  const composerRef = useRef<HTMLDivElement>(null);
  const [composerResizing, setComposerResizing] = useState(false);
  // Divider mousedown position: distinguishes a click (toggle) from a drag
  // (resize) on the composer divider.
  const resizeStartRef = useRef<{ x: number; y: number } | null>(null);

  // Composer popover menus (向上弹出)：文件/引用 (+).
  const [refMenuOpen, setRefMenuOpen] = useState(false);
  const [recentEntries, setRecentEntries] = useState<RecentEntry[]>([]);
  const refAnchorRef = useRef<HTMLSpanElement>(null);
  const refMenuRef = useRef<HTMLDivElement>(null);

  // Stale-response guard for async fs_list fetches in the `@` menu.
  const atReqRef = useRef(0);
  const slashReqRef = useRef(0);
  const slashCatalogRef = useRef<{
    agentId: string;
    items: SlashItem[];
  } | null>(null);
  // Per-agent+parent cache of the static second-level items (`/mcp`, `/agents`,
  // ...). Keyed by agent so switching tabs never leaks another session's data.
  const slashChildrenRef = useRef<Map<string, SlashItem[]>>(new Map());
  // Guards against a stale child fetch overwriting a newer level (Bug pattern:
  // select `/model`, quickly Esc, select `/mcp` → the `/model` response must
  // not paint over `/mcp`'s picker).
  const slashChildReqRef = useRef(0);
  // The Composer textarea is intentionally uncontrolled for low-latency typing.
  // Keep a separate draft per terminal and swap the DOM value on tab changes.
  const terminalDraftsRef = useRef<Map<string, ComposerDraftState>>(new Map());
  const draftOwnerRef = useRef<string | null>(null);
  // Guards against double-insert when both the DOM drop handler and the Tauri
  // drag-drop event observe the same drop.
  const dropHandledRef = useRef(false);
  // Nesting counter (dragenter/dragleave fire when crossing child boundaries).
  const dragDepthRef = useRef(0);
  // Guards against double-send on rapid Enter (Bug 3).
  const sendingRef = useRef(false);
  // Latest `handleSend`, so `selectSlashItem` (declared earlier in the body)
  // can auto-send a leaf selection without a stale closure or a TDZ access to
  // the later `handleSend` const.
  const handleSendRef = useRef<() => void>(() => {});
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

  // Phase 5 tab classification: structured agents render their own composer in
  // AgentPanel; legacy PTY Agent sessions (claude/codex/opencode) are read-only
  // EOL; only bash terminals (and the no-tab "create on send" case) use this
  // Composer's input.
  const activeRuntime = targetAgentId ? agents.get(targetAgentId)?.runtime : undefined;
  const activeIsStructured = activeTab?.type === "structured";
  const activeIsLegacyAgent =
    activeTab?.type === "agent" &&
    !!activeRuntime &&
    activeRuntime !== "bash" &&
    activeRuntime !== "bash-rc";

  // Effective send target: a Tab-cycled pin wins; otherwise follow the active
  // terminal. Falls back to null (spawn a new session on send) when the active
  // tab carries no agent. A cycled terminal whose session has since ended or
  // been deleted falls back to following the active tab rather than sending
  // into a dead session.
  const effectiveTarget: ComposerTarget | null = useMemo(() => {
    if (cycleTarget?.kind === "agent") {
      const alive = agents.get(cycleTarget.agentId);
      if (!alive || alive.status === "done" || alive.status === "failed") {
        return targetAgentId ? { kind: "agent", agentId: targetAgentId } : null;
      }
    }
    return cycleTarget ??
      (targetAgentId ? { kind: "agent", agentId: targetAgentId } : null);
  }, [cycleTarget, agents, targetAgentId]);

  useEffect(() => {
    // Skills are session/runtime/cwd-specific. Never carry a previous agent's
    // catalog into a newly selected tab.
    atReqRef.current += 1;
    slashReqRef.current += 1;
    slashCatalogRef.current = null;
    slashChildrenRef.current.clear();
    setAtMenu(null);
    setSlashMenuStack([]);
    // A Tab-cycled send target follows the newly-active terminal again.
    setCycleTarget(null);
  }, [targetAgentId]);

  useEffect(() => {
    // Keep the highlighted file visible while navigating a long `@` result list.
    const activeItem = atMenuRef.current?.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]'
    );
    activeItem?.scrollIntoView({ block: "nearest" });
  }, [atMenu?.idx, atMenu?.items]);

  const topSlashLevel = slashMenuStack[slashMenuStack.length - 1];

  useEffect(() => {
    // Long native command lists (notably Claude) overflow the picker.
    // Keep keyboard navigation synchronized with the visible scroll region.
    const activeItem = slashMenuRef.current?.querySelector<HTMLElement>(
      '[role="option"][aria-selected="true"]'
    );
    activeItem?.scrollIntoView({ block: "nearest" });
  }, [topSlashLevel?.idx, topSlashLevel?.items]);

  // Phase 5: the per-session permission/model/thinking controls (PTY key
  // injection into claude/codex/opencode TUIs) were retired — structured
  // agents configure through AgentPanel, and legacy PTY agents are read-only
  // EOL. This Composer only serves bash terminals (plain input + @ files).


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

  // ── F1 → toggle focus between the composer input and the terminal ──────
  // Composer owns the F1 window listener (it always mounts, and it knows both
  // the textarea and its open/closed state). When the input holds focus we hand
  // focus to the active tab's terminal via a store directive; otherwise (focus
  // in the terminal, a sidebar, or elsewhere) we focus the input. A collapsed
  // composer hides its input (`display:none`, unfocusable), so F1 then routes
  // to the terminal instead of no-oping. Dismiss any open popover first so
  // focus can't straddle the two areas.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "F1") return;
      e.preventDefault();
      const el = textareaRef.current;
      const inputFocused = el !== null && document.activeElement === el;
      const st = useStore.getState();
      // The active tab is where the terminal would live: an agent tab with a
      // session renders an XTermPanel; editor/diff tabs and placeholder agent
      // tabs have no terminal to hand focus to.
      const activeTab = st.tabs.find((t) => t.id === st.activeTabId);
      const hasTerminal = activeTab?.type === "agent" && !!activeTab.agentId;
      // Dismiss any open popover first so focus can't straddle the two areas.
      setAtMenu(null);
      setSlashMenuStack([]);
      setRefMenuOpen(false);
      if (inputFocused) {
        if (hasTerminal) st.requestFocus("terminal");
        // No terminal to move to — leave focus in the input.
      } else if (el && st.composerOpen) {
        el.focus();
      } else if (hasTerminal) {
        st.requestFocus("terminal");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;

    const nextOwner = targetAgentId ?? "__untargeted__";
    const previousOwner = draftOwnerRef.current;
    if (previousOwner !== null && previousOwner !== nextOwner) {
      terminalDraftsRef.current.set(previousOwner, {
        value: el.value,
        selectionStart: el.selectionStart ?? el.value.length,
        selectionEnd: el.selectionEnd ?? el.value.length,
      });
    }

    const nextDraft = terminalDraftsRef.current.get(nextOwner);
    el.value = nextDraft?.value ?? "";
    const caret = Math.min(nextDraft?.selectionStart ?? el.value.length, el.value.length);
    const selectionEnd = Math.min(nextDraft?.selectionEnd ?? caret, el.value.length);
    el.setSelectionRange(caret, selectionEnd);
    draftOwnerRef.current = nextOwner;

    resizeTextarea(el);
    setIsBangInput(el.value.trimStart().startsWith("!"));
    setHasInput(el.value.trim().length > 0);
    // Up/down history navigation starts independently after a terminal switch.
    useStore.setState({ draftIndex: -1 });
  }, [resizeTextarea, targetAgentId]);

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
      // A todo tag dropped into the composer inserts its text at the cursor
      // (non-destructive — the tag stays in 待分配 until assigned to a session).
      if (e.dataTransfer.types.includes(TODO_DRAG_MIME)) {
        const tagId = e.dataTransfer.getData(TODO_DRAG_MIME);
        const tag = useStore.getState().todos.find((t) => t.id === tagId);
        if (tag) insertText(tag.text + " ");
        dropHandledRef.current = true;
        dragDepthRef.current = 0;
        setDragHover(false);
        return;
      }
      // Extract absolute paths straight from the DOM dataTransfer. On Tauri
      // v2 + WebKitGTK the legacy `File.path` is gone and the Tauri drag-drop
      // event can be unreliable, so this is the primary source (the file
      // manager's `text/uri-list`, plus `text/plain` for app-internal drags).
      const paths = pathsFromDataTransfer(e.dataTransfer);
      if (paths.length) {
        appendPaths(paths);
        dropHandledRef.current = true;
        dragDepthRef.current = 0;
        setDragHover(false);
      }
      // No path at all → leave dragDepthRef/dropHandledRef untouched so the
      // Tauri drag-drop event (which fires next) can still detect the composer.
    },
    [appendPaths, insertText]
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
        atReqRef.current += 1;
        setAtMenu(null);
        return;
      }
      const query = before.slice(lastAt + 1);
      // A space / newline ends the `@` mention.
      if (/\s/.test(query)) {
        atReqRef.current += 1;
        setAtMenu(null);
        return;
      }

      const cwd = resolveTargetCwd();
      if (!cwd) {
        atReqRef.current += 1;
        setAtMenu(null);
        return;
      }

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

  // ── Runtime-aware `/` skill / command autocomplete ───────────
  const slashContext = useCallback((el: HTMLTextAreaElement) => {
    const pos = el.selectionStart ?? el.value.length;
    const before = el.value.slice(0, pos);
    // Provider slash commands are meaningful only as the leading token. Allow
    // indentation, but dismiss once arguments/ordinary whitespace are typed.
    const match = before.match(/^(\s*)\/([^\s]*)$/);
    if (!match) return null;
    return { anchor: match[1].length, query: match[2] };
  }, []);

  const filterSlashItems = useCallback((items: SlashItem[], query: string) => {
    const needle = query.toLocaleLowerCase();
    if (!needle) return items;
    return items.filter((item) =>
      `${item.name} ${item.invocation} ${item.description}`
        .toLocaleLowerCase()
        .includes(needle)
    );
  }, []);

  const handleSlashAuto = useCallback(
    async (el: HTMLTextAreaElement) => {
      const context = slashContext(el);
      if (!context) {
        slashReqRef.current += 1;
        setSlashMenuStack([]);
        return;
      }
      const agentId = targetAgentId;
      if (!agentId) {
        slashReqRef.current += 1;
        setSlashMenuStack([]);
        return;
      }

      const rootLevel = (items: SlashItem[], loading: boolean): SlashMenuState => ({
        anchor: context.anchor,
        rollbackTo: -1,
        parent: null,
        items,
        idx: 0,
        loading,
        query: context.query,
      });

      const cached = slashCatalogRef.current;
      if (cached?.agentId === agentId) {
        setSlashMenuStack([rootLevel(cached.items, false)]);
        return;
      }

      setSlashMenuStack([rootLevel([], true)]);
      const req = ++slashReqRef.current;
      try {
        const items =
          (await invoke<SlashItem[]>("agent_list_slash_items", { id: agentId })) ?? [];
        if (req !== slashReqRef.current || targetAgentId !== agentId) return;
        slashCatalogRef.current = { agentId, items };
        // Re-derive the trigger from the current text: the user may have typed
        // more while the catalog was loading, or deleted past `/` entirely.
        const currentEl = textareaRef.current;
        const current = currentEl ? slashContext(currentEl) : null;
        if (!current) {
          setSlashMenuStack([]);
          return;
        }
        setSlashMenuStack([
          {
            anchor: current.anchor,
            rollbackTo: -1,
            parent: null,
            items,
            idx: 0,
            loading: false,
            query: current.query,
          },
        ]);
      } catch {
        if (req === slashReqRef.current) {
          setSlashMenuStack([rootLevel([], false)]);
        }
      }
    },
    [slashContext, targetAgentId]
  );

  /** Patch the deepest open level without touching the rest of the stack. */
  const updateTopLevel = useCallback((patch: Partial<SlashMenuState>) => {
    setSlashMenuStack((stack) => {
      if (stack.length === 0) return stack;
      const next = stack.slice();
      next[next.length - 1] = { ...next[next.length - 1], ...patch };
      return next;
    });
  }, []);

  /** Apply fetched children to the top level, but only while it is still the
      child level that requested them (guards against a stale response painting
      over a different picker after Esc/descend raced). The query is re-derived
      from the live textarea so text typed during the brief loading window is
      not lost, and a fresh load resets the selection. */
  const patchTopIfParent = useCallback(
    (parent: SlashItem, patch: Partial<SlashMenuState>) => {
      setSlashMenuStack((stack) => {
        if (stack.length === 0) return stack;
        const top = stack[stack.length - 1];
        if (top.parent?.name !== parent.name) return stack;
        const el = textareaRef.current;
        const query = el ? el.value.slice(top.anchor) : top.query;
        const next = stack.slice();
        next[next.length - 1] = { ...top, ...patch, query, idx: 0 };
        return next;
      });
    },
    []
  );

  /** Fetch the static second-level items for a parent, honoring the cache. */
  const fetchSlashChildren = useCallback(
    async (agentId: string, parent: SlashItem) => {
      const req = ++slashChildReqRef.current;
      const cacheKey = `${agentId}:${parent.name}`;
      const cached = slashChildrenRef.current.get(cacheKey);
      if (cached) {
        patchTopIfParent(parent, { items: cached, loading: false });
        return;
      }
      try {
        const items =
          (await invoke<SlashItem[]>("agent_list_slash_children", {
            id: agentId,
            parent: parent.name,
          })) ?? [];
        if (req !== slashChildReqRef.current) return; // stale
        slashChildrenRef.current.set(cacheKey, items);
        patchTopIfParent(parent, { items, loading: false });
      } catch {
        if (req === slashChildReqRef.current) {
          patchTopIfParent(parent, { items: [], loading: false });
        }
      }
    },
    [patchTopIfParent]
  );

  /** Complete/descend into a menu item. Leaves send the completed line straight
      to the terminal; commands with children push a loading level and fetch
      their options from the backend. */
  const selectSlashItem = useCallback(
    (item: SlashItem) => {
      if (slashMenuStack.length === 0) return;
      const el = textareaRef.current;
      if (!el) return;
      const level = slashMenuStack[slashMenuStack.length - 1];
      const insert = `${item.invocation} `;
      const beforeLen = el.value.length;

      if (slashMenuStack.length === 1) {
        // Root level: replace the `/query` token (query + trigger `/`) with the
        // selected invocation. The pre-edit text length is the rollback point
        // for the child level, if one opens.
        const replacedLength = level.query.length + 1;
        el.value =
          el.value.slice(0, level.anchor) +
          insert +
          el.value.slice(level.anchor + replacedLength);
      } else {
        // Child level: the typed filter (everything after this level's anchor)
        // is replaced by the selected invocation — otherwise `/model op` +
        // `claude-opus-5` would concatenate into `/model opclaude-opus-5`.
        const filterLength = Math.max(0, el.value.length - level.anchor);
        el.value =
          el.value.slice(0, level.anchor) +
          insert +
          el.value.slice(level.anchor + filterLength);
      }
      el.selectionStart = el.selectionEnd = el.value.length;
      el.focus();
      resizeTextarea(el);
      setHasInput(true);

      if (item.has_children) {
        setSlashMenuStack((stack) => [
          ...stack,
          {
            anchor: el.value.length,
            rollbackTo: beforeLen,
            parent: item,
            items: [],
            idx: 0,
            loading: true,
            query: "",
          },
        ]);
        const agentId = targetAgentId;
        if (agentId) void fetchSlashChildren(agentId, item);
      } else {
        // Final-level selection: the completed line (`/model claude-opus-5 `)
        // goes straight to the terminal. `handleSend` reads the textarea
        // synchronously, clears it, and sends — same path as a manual Enter.
        setSlashMenuStack([]);
        handleSendRef.current();
      }
    },
    [slashMenuStack, targetAgentId, resizeTextarea, fetchSlashChildren]
  );

  /** Esc / ← 返回: roll the text back past the level-opening invocation and pop
      one level. Root Esc just closes the whole menu. */
  const popSlashLevel = useCallback(() => {
    if (slashMenuStack.length === 0) return;
    if (slashMenuStack.length === 1) {
      setSlashMenuStack([]);
      return;
    }
    const top = slashMenuStack[slashMenuStack.length - 1];
    const el = textareaRef.current;
    if (el && top.rollbackTo >= 0 && top.rollbackTo <= el.value.length) {
      el.value = el.value.slice(0, top.rollbackTo);
      el.selectionStart = el.selectionEnd = top.rollbackTo;
      el.focus();
      resizeTextarea(el);
      setHasInput(el.value.trim().length > 0);
    }
    setSlashMenuStack((stack) => stack.slice(0, -1));
  }, [slashMenuStack, resizeTextarea]);

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

  // ── Send target (Tab cycle: visible terminals ↔ 待分配) ────────
  /** Advance the composer's send target through the terminals VISIBLE in the
   *  content area (the split leaves when in split view, else just the active
   *  tab) then 待分配, wrapping around. Tabs parked in the bar but not rendered
   *  in this area — other split panes' sessions, or open tabs not currently
   *  shown — stay out of the cycle. A bare Tab from "follow the active tab"
   *  starts at the next terminal after the active one (or the first terminal /
   *  待分配 when the active tab isn't a live terminal). */
  const cycleSendTarget = useCallback(() => {
    // The visible set is the split tree's leaves in split view, otherwise the
    // active tab alone. Ended/failed sessions (status done/failed) keep a row
    // in the sidebar but are no longer open terminals, so they stay out of the
    // cycle. Visible order is the natural 终端1/终端2 ordering the user sees.
    const visibleIds = splitTree
      ? splitLeafTabIds(splitTree)
      : activeTabId
        ? [activeTabId]
        : [];
    const terminalIds = visibleIds
      .map((id) => tabs.find((t) => t.id === id))
      .filter((t) => t && t.type === "agent" && !!t.agentId)
      .map((t) => t!.agentId as string)
      .filter((id) => {
        const a = agents.get(id);
        return a && a.status !== "done" && a.status !== "failed";
      });
    const slots: ComposerTarget[] = [
      ...terminalIds.map((agentId) => ({ kind: "agent" as const, agentId })),
      { kind: "todo" as const },
    ];
    let cur = -1;
    if (cycleTarget) {
      cur = slots.findIndex((s) =>
        s.kind === "todo"
          ? cycleTarget.kind === "todo"
          : cycleTarget.kind === "agent" &&
            s.kind === "agent" &&
            s.agentId === cycleTarget.agentId
      );
    } else if (targetAgentId) {
      cur = slots.findIndex(
        (s) => s.kind === "agent" && s.agentId === targetAgentId
      );
    }
    setCycleTarget(slots[(cur + 1) % slots.length]);
  }, [splitTree, activeTabId, tabs, agents, targetAgentId, cycleTarget]);

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
    setAtMenu(null);
    setSlashMenuStack([]);

    // 待分配 target: the input becomes a task tag instead of a prompt. The `!`
    // 终端直发 marker is stripped exactly like an agent send — it is a Composer
    // directive, not message content.
    if (effectiveTarget?.kind === "todo") {
      addTodo(text);
      return;
    }

    sendingRef.current = true;
    let agentId =
      effectiveTarget?.kind === "agent" ? effectiveTarget.agentId : targetAgentId;
    let justSpawned = false;
    try {
      if (!agentId) {
        // Phase 5: a new session defaults to the structured backend.
        agentId = await createDefaultAgent();
        justSpawned = true;
      }

      // `sendPromptToAgent` (shared with the todo-tag drop targets) ensures a
      // live channel (resuming restored sessions), then gives a freshly-spawned
      // /resumed CLI TUI time to attach its input loop before injecting the
      // message. A fixed 800ms can be too short on slow machines / cold claude
      // starts (first instruction typed before the TUI is reading stdin →
      // dropped or eaten by the shell prompt).
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
      await sendPromptToAgent(agentId, agentInput, { waitForTui: justSpawned });
    } catch (err) {
      console.error("Failed to send to agent:", err);
    } finally {
      // Release the in-flight guard so the next Enter can send again.
      sendingRef.current = false;
    }
  }, [
    effectiveTarget,
    targetAgentId,
    resizeTextarea,
    pushDraft,
    addTodo,
  ]);

  // Keep the auto-send bridge fresh whenever `handleSend` is recreated.
  useEffect(() => {
    handleSendRef.current = handleSend;
  }, [handleSend]);

  // ── Keyboard ──────────────────────────────────────────────────
  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (slashMenuStack.length > 0) {
        const level = slashMenuStack[slashMenuStack.length - 1];
        const visible = level.query
          ? filterSlashItems(level.items, level.query)
          : level.items;
        const count = visible.length;
        if (e.key === "ArrowDown") {
          e.preventDefault();
          if (count) updateTopLevel({ idx: (level.idx + 1) % count });
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          if (count) updateTopLevel({ idx: (level.idx - 1 + count) % count });
          return;
        }
        if (e.key === "Enter" || e.key === "Tab") {
          e.preventDefault();
          const item = visible[Math.min(level.idx, count - 1)];
          if (item) selectSlashItem(item);
          return;
        }
        // Esc pops one level (root Esc closes the whole menu); ArrowLeft is a
        // mouse-free way to climb back to the parent level.
        if (e.key === "Escape" || e.key === "ArrowLeft") {
          e.preventDefault();
          popSlashLevel();
          return;
        }
      }

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
        // Tab: 在打开的终端 + 待分配 之间循环发送目标（见 cycleSendTarget）.
        cycleSendTarget();
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
        // 关闭文件引用菜单（Phase 5：模型/权限/思考菜单与终端式中断已随
        // 旧版 PTY Agent 的按键注入一并移除）。
        if (refMenuOpen) setRefMenuOpen(false);
      }
    },
    [
      atMenu,
      slashMenuStack,
      filterSlashItems,
      updateTopLevel,
      selectSlashItem,
      popSlashLevel,
      insertAtItem,
      handleSend,
      cycleSendTarget,
      navigateDraft,
      refMenuOpen,
    ]
  );

  const handleInput = useCallback(
    (e: FormEvent<HTMLTextAreaElement>) => {
      const el = e.currentTarget;
      resizeTextarea(el);
      setIsBangInput(el.value.trimStart().startsWith("!"));
      setHasInput(el.value.trim().length > 0);
      // Typing dismisses the file-reference popover.
      setRefMenuOpen(false);
      if (slashMenuStack.length > 0) {
        // At the root level the trigger is live text: deleting the `/` or
        // typing a space invalidates the `/query` token and must close the
        // menu (mirrors the pre-stack behavior). Child levels sit on committed
        // parent text, so only their filter is re-derived.
        if (slashMenuStack.length === 1 && !slashContext(el)) {
          slashReqRef.current += 1;
          setSlashMenuStack([]);
          return;
        }
        // A menu level is open: the deepest level's filter is whatever text
        // follows its anchor. Re-filter live without re-fetching the catalog.
        setSlashMenuStack((stack) => {
          if (stack.length === 0) return stack;
          const next = stack.slice();
          const last = next[next.length - 1];
          if (last.loading) return stack;
          next[next.length - 1] = {
            ...last,
            query: el.value.slice(last.anchor),
            idx: 0,
          };
          return next;
        });
        return;
      }
      handleAtAuto(el);
      void handleSlashAuto(el);
    },
    [resizeTextarea, handleAtAuto, handleSlashAuto, slashMenuStack]
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

  // ── Phase 5 gates ──────────────────────────────────────────────
  // Structured agents have their own composer inside AgentPanel — this global
  // bar adds nothing for them. Legacy PTY Agent sessions (claude/codex/opencode)
  // are read-only EOL: the bar shows the retirement notice instead of an input,
  // and no keystroke / prompt injection is possible (the backend refuses
  // `agent_write` for them as well).
  if (activeIsStructured) {
    return (
      <div className="composer composer-gated">
        <div className="composer-gate-note">
          <Icon name="bot" size={13} style={{ marginRight: 6 }} />
          结构化 Agent 会话请在面板内直接输入（AgentPanel 自带输入框与配置条）。
        </div>
      </div>
    );
  }
  if (activeIsLegacyAgent) {
    return (
      <div className="composer composer-gated">
        <div className="composer-gate-note composer-gate-eol">
          <Icon name="triangle-alert" size={13} style={{ marginRight: 6 }} />
          该会话为旧版 PTY Agent，已进入只读兼容模式（EOL）。请通过「新建 Agent」创建结构化 Agent 会话以继续工作。
        </div>
      </div>
    );
  }

  return (
    <div
      ref={composerRef}
      className={`composer${!composerOpen ? " composer-collapsed" : ""}`}
      style={composerOpen && effH ? { height: effH } : undefined}
    >
      {/* Height divider: drag to resize, double-click to reset, hover button to
          collapse/expand (always rendered so a collapsed composer can be
          reopened). */}
      <div
        className={`composer-resize${composerResizing ? " active" : ""}`}
        title="单击收起/展开 · 拖拽调整高度 · 双击恢复默认高度"
        onMouseDown={(e) => {
          resizeStartRef.current = { x: e.clientX, y: e.clientY };
          startComposerResize(e);
        }}
        onClick={(e) => {
          // A click (no movement) toggles the composer; a drag resizes instead.
          const start = resizeStartRef.current;
          resizeStartRef.current = null;
          if (
            start &&
            Math.hypot(e.clientX - start.x, e.clientY - start.y) > 5
          ) {
            return;
          }
          toggleComposer();
        }}
        onDoubleClick={resetComposerH}
      >
        <button
          className="resize-collapse"
          title={composerOpen ? "收起输入区" : "展开输入区"}
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            toggleComposer();
          }}
        >
          <Icon name={composerOpen ? "chevron-down" : "chevron-up"} size={10} />
        </button>
      </div>
      {/* Target line */}
      <div className="composer-target">
        <span>
          <Icon name="arrow-right" size={12} style={{ marginRight: 4 }} />
          {effectiveTarget?.kind === "todo" ? (
            <span className="composer-target-todo" title="内容将添加到待分配">
              待分配
            </span>
          ) : effectiveTarget?.kind === "agent" ? (
            <>
              agent: {agents.get(effectiveTarget.agentId)?.title ?? "agent"}
            </>
          ) : (
            "(无标签)"
          )}
        </span>
        <ContextWindowMeter
          agentId={
            effectiveTarget?.kind === "agent"
              ? effectiveTarget.agentId
              : undefined
          }
        />
        <span className="composer-target-right">
          {isBangInput && effectiveTarget?.kind !== "todo" && (
            <span className="composer-bang">
              <Icon name="zap" size={12} style={{ marginRight: 4 }} />
              终端直发
            </span>
          )}
          <span
            className="composer-f1-hint"
            title="Tab 在打开的终端与待分配之间切换发送目标"
          >
            <kbd>Tab</kbd> 切换目标
          </span>
          <span
            className="composer-f1-hint"
            title="F1 在输入框与终端之间切换焦点"
          >
            <kbd>F1</kbd> 切换焦点
          </span>
        </span>
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
            placeholder={
              effectiveTarget?.kind === "todo"
                ? "添加待办…（Enter 加入待分配）"
                : "发消息…（/ 命令 · @ 文件 · ! 终端 · 拖入文件）"
            }
            rows={4}
            onKeyDown={handleKeyDown}
            onInput={handleInput}
          />
          <button
            className="ul-send-btn"
            title={
              effectiveTarget?.kind === "todo"
                ? "添加到待分配（Enter）"
                : "发送消息（Enter）"
            }
            onClick={() => handleSend()}
            disabled={sendingRef.current || !hasInput}
          >
            发送
          </button>
        </div>
      </div>

      {/* `@` file autocomplete menu */}
      {atMenu && (
        <div ref={atMenuRef} className="composer-at-menu" role="listbox">
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

      {/* Runtime-aware native command / skill menu. `/` is only the trigger:
          each row inserts the syntax required by the selected agent. Commands
          with children (has_children) push a second-level picker. */}
      {slashMenuStack.length > 0 &&
        (() => {
          const level = slashMenuStack[slashMenuStack.length - 1];
          const visible = level.query
            ? filterSlashItems(level.items, level.query)
            : level.items;
          const breadcrumb = slashMenuStack
            .map((l) => l.parent?.name)
            .filter((n): n is string => Boolean(n));
          return (
            <div
              ref={slashMenuRef}
              className="composer-at-menu composer-slash-menu"
              role="listbox"
            >
              <div className="composer-slash-head">
                <span>
                  {breadcrumb.length > 0 ? (
                    <>
                      <span className="composer-slash-breadcrumb">
                        /
                        {breadcrumb.map((seg, i) => (
                          <span key={`${seg}-${i}`}>
                            {i > 0 && (
                              <Icon name="chevron-right" size={10} style={{ margin: "0 3px" }} />
                            )}
                            {seg}
                          </span>
                        ))}
                      </span>{" "}
                      {activeRuntime ?? "终端"}
                    </>
                  ) : (
                    activeRuntime ?? "终端"
                  )}
                </span>
                <span>内置命令 / 自定义命令 / 技能</span>
              </div>
              {slashMenuStack.length > 1 && (
                <div
                  className="composer-slash-back"
                  role="option"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={popSlashLevel}
                >
                  <Icon name="arrow-left" size={12} style={{ marginRight: 4 }} />
                  返回
                </div>
              )}
              {level.loading ? (
                <div className="composer-slash-empty">正在读取…</div>
              ) : visible.length === 0 ? (
                <div className="composer-slash-empty">没有匹配的命令或技能</div>
              ) : (
                visible.map((item, i) => (
                  <div
                    key={`${item.invocation}:${item.source}`}
                    role="option"
                    aria-selected={i === level.idx}
                    className={`composer-at-item composer-slash-item${
                      i === level.idx ? " active" : ""
                    }`}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => selectSlashItem(item)}
                  >
                    <span className="composer-slash-invocation">
                      {item.invocation}
                      {item.has_children && (
                        <Icon name="chevron-right" size={10} style={{ marginLeft: 4 }} />
                      )}
                    </span>
                    <span className="composer-slash-description">
                      {item.description || (item.kind === "skill" ? "加载 Agent 技能" : "运行命令")}
                    </span>
                    <span className="composer-slash-source">{item.source}</span>
                  </div>
                ))
              )}
            </div>
          );
        })()}

      {/* Actions */}
      <div className="composer-actions">
        <span className="cmp-pop" ref={refAnchorRef}>
          <span
            className="act-btn"
            title="插入文件引用 / 最近文件"
            onClick={() => setRefMenuOpen((o) => !o)}
          >
            +
          </span>
          {refMenuOpen && (
            <div className="cmp-menu" ref={refMenuRef} role="menu">
              <div className="cmp-menu-label">插入文件/引用</div>
              <div className="cmp-menu-item" onClick={handlePickFile}>
                <span className="cmp-menu-name">
                  <Icon name="file-text" size={13} style={{ marginRight: 6 }} /> 选择文件…
                </span>
              </div>
              <div className="cmp-menu-item" onClick={handlePasteRef}>
                <span className="cmp-menu-name">
                  <Icon name="link" size={13} style={{ marginRight: 6 }} /> 粘贴引用/路径
                </span>
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

        <span className="act-sep" />
      </div>
    </div>
  );
}
