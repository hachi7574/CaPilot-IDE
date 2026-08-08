import { create } from "zustand";
import { invoke, Channel } from "@tauri-apps/api/core";

// ── Types ───────────────────────────────────────────────────────

export type AgentRole = "master" | "worker" | "standalone";
export type AgentStatus =
  | "idle"
  | "running"
  | "waiting_input"
  | "busy"
  | "done"
  | "failed";
export type PermissionMode = "ask" | "auto" | "yolo";
export type Speed = "high" | "mid" | "fast" | "auto";
/** UI font size preset. Base `"s"` is the smallest; larger presets scale the
 *  CSS `--fs-*` tokens. */
export type FontScale = "s" | "m" | "l" | "xl" | "xxl";

export interface AgentInfo {
  id: string;
  workspace_id?: string | null;
  requires_attention?: boolean;
  attention_reason?: "finished" | "error" | null;
  /** Stable owning project supplied from persistence; cwd is execution-only. */
  project?: string;
  runtime: string;
  role: AgentRole;
  status: AgentStatus;
  title: string;
  cwd: string;
  pid: number | null;
  /** Permission mode this session runs under ("ask" | "auto" | "yolo"). */
  mode?: string;
  /** Speed tier this session runs under ("high" | "mid" | "fast" | "auto"). */
  speed?: string;
  /** Selected model id, or null/undefined for the runtime default. */
  model?: string | null;
  /** Session creation epoch-ms — the sidebar `tm-time` count-up is anchored to
   *  this real timestamp (NOT the activity heartbeat). Restored sessions carry
   *  the DB `created_at`; fresh spawns get `Date.now()` at spawn. */
  createdAt?: number;
}

export interface RuntimeInfo {
  id: string;
  name: string;
  available: boolean;
  authenticated: boolean;
  models?: { id: string; name: string; provider: string }[];
}

export interface Tab {
  id: string;
  type: "agent" | "editor" | "diff";
  agentId?: string;
  /** Editor: absolute file path. Diff: the "new" (worktree/index) side path,
   *  used for project grouping in the tab bar. */
  filePath?: string;
  /** Diff tabs carry a snapshot of the two sides at open time. */
  diffOld?: string;
  diffNew?: string;
  title: string;
}

/** Matches the Rust `AgentSessionRecord` (snake_case keys). */
export interface RestoredSession {
  id: string;
  workspace_id: string | null;
  requires_attention: boolean;
  attention_reason: "finished" | "error" | null;
  project: string;
  role: AgentRole;
  runtime: string;
  resume_key: string | null;
  cwd: string;
  title: string;
  status: string;
  mode: string;
  speed: string;
  model: string | null;
  created_at: number;
  updated_at: number;
}

export interface WorkerInfo {
  id: string;
  title: string;
  runtime: string;
  status: string;
  last_task: string | null;
}

export interface WorkerReport {
  worker: string;
  summary: string;
  level: string;
  ts: number;
}

export interface EspStatus {
  connected: boolean;
  kind: "ble" | "usb" | "wifi" | null;
  name: string | null;
  address: string | null;
  rssi: number | null;
  battery_pct: number | null;
  battery_mv: number | null;
  last_seen_ms: number | null;
}

/** One agent's resource snapshot from `resource://sample` (DevPlan §10). */
export interface AgentResource {
  agent_id: string;
  cpu_pct: number;
  mem_bytes: number;
}

/** A buffered CPU/MEM history point (for the sparkline curve). */
export interface ResourcePoint {
  cpu_pct: number;
  mem_bytes: number;
}

// ── Helpers ─────────────────────────────────────────────────────

/** Max buffered bytes per agent before XTermPanel attaches. */
const MAX_OUTPUT_BUFFER = 2_000_000;

/**
 * Derive the workspace project name from an agent cwd — mirrors the sidebar's
 * `projectOf` so `removeProject` matches the exact grouping the tree uses.
 */
function projectOfCwd(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  // Master-group terminals live in ~/CaPilot/Master.
  if (cwd.endsWith("/CaPilot/Master") || cwd.includes("/CaPilot/Master/")) {
    return "master";
  }
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/**
 * Create a Tauri Channel that buffers every event immediately, so no PTY
 * output is lost in the race between spawn and the terminal mounting.
 * `flush(agentId)` routes the buffered + future data into the store buffer.
 */
export function createBufferedChannel(): {
  channel: Channel<number[]>;
  flush: (agentId: string) => void;
} {
  const pending: number[] = [];
  const channel = new Channel<number[]>();
  channel.onmessage = (data) => {
    pending.push(...data);
  };
  return {
    channel,
    flush: (agentId: string) => {
      if (pending.length) {
        useStore.getState().appendAgentOutput(agentId, pending);
        pending.length = 0;
      }
      channel.onmessage = (data) => {
        useStore.getState().appendAgentOutput(agentId, data);
      };
    },
  };
}

// ── Agent count-up anchor ────────────────────────────────────────
// The sidebar `tm-time` is a "time since session creation" counter anchored to
// the agent's `createdAt` (the persisted DB `created_at` on restore, `Date.now()`
// on a fresh spawn). It deliberately is NOT a last-activity heartbeat: Claude's
// TUI repaints and buffered output would keep resetting a "last activity" stamp
// to "刚刚" even for a session idle for hours. createdAt is a real, monotonic,
// persisted timestamp, so the count-up always advances.

// ── Store ────────────────────────────────────────────────────────

interface AppState {
  // Agents
  agents: Map<string, AgentInfo>;
  agentChannels: Map<string, Channel<number[]>>;
  /** Output buffered before a terminal attached (and between mounts). */
  agentOutputs: Map<string, number[]>;
  masterAgentId: string | null;
  /** Agent ids whose next terminal mount should force a resume (sidebar
   *  "已结束" reopen). Ended (`done`) sessions never auto-resume otherwise. */
  resumeOnOpen: Set<string>;
  /** Tombstones for ids removed via removeAgent: guards against a stale
   *  in-flight `agent_resume` resolving after the session was closed/deleted
   *  (close/resume race). `addAgent` ignores tombstoned ids so a zombie agent
   *  (status running, dead channel, no `agent://exited` ever coming) can't
   *  reappear and be unclosable. */
  closedAgentIds: Set<string>;

  // Runtimes
  runtimes: RuntimeInfo[];

  // Orchestration
  workerInfos: WorkerInfo[];
  reports: WorkerReport[];
  smartReturn: boolean;

  // UI tabs
  tabs: Tab[];
  activeTabId: string | null;

  // Composer
  composerTarget: "agent" | "master";
  composerOpen: boolean;
  permissionMode: PermissionMode;
  speed: Speed;
  /** Runtime model id chosen via composer `[模型↑]` (null = runtime default). */
  selectedModel: string | null;
  workerMode: boolean;
  draftHistory: string[];
  draftIndex: number;

  // Split layout (DevPlan §3.1.2) — paneA/paneB hold tab ids; a null pair
  // means the default single-panel view.
  splitPaneA: string | null;
  splitPaneB: string | null;
  splitDirection: "row" | "column" | null;
  /** 0..1 fraction of the container taken by paneA. */
  splitRatio: number;
  /** Tab id currently being dragged (for edge-drop feedback). */
  draggedTabId: string | null;

  // Worker lock (DevPlan §4.6) — agent id currently unlocked (allows a send /
  // terminal input) despite being a worker.
  workerUnlockId: string | null;

  // Projects (workspace dirs under ~/CaPilot/workspaces/<name>)
  projects: string[];
  /** project name → absolute root path (from list_projects / create_project). */
  projectRoots: Record<string, string>;
  /** Single-select focused project; null = unfocused (tab bar shows all tabs). */
  focusedProject: string | null;

  // Sidebars
  leftSidebarOpen: boolean;
  rightSidebarOpen: boolean;
  leftWidth: number;
  rightWidth: number;

  // Composer height
  /** Composer height (px); null = follow the right sidebar's master report. */
  composerH: number | null;
  /** Measured height of the master report card in the right sidebar (px). */
  masterReportH: number;

  // ESP
  espStatus: EspStatus;
  espConnecting: boolean;

  // Resource monitor (DevPlan §10)
  agentResources: Map<string, ResourcePoint>;
  resourceHistory: Map<string, ResourcePoint[]>;

  // Onboarding
  onboarded: boolean;

  // UI font size preset ("s" | "m" | "l" | "xl" | "xxl"); base = smallest.
  fontScale: FontScale;

  // Actions
  addAgent: (info: AgentInfo, channel: Channel<number[]> | null, createdAtTs?: number) => void;
  removeAgent: (id: string) => void;
  updateAgentStatus: (id: string, status: AgentStatus) => void;
  updateAgentAttention: (id: string, reason: "finished" | "error" | null) => void;
  appendAgentOutput: (id: string, data: number[]) => void;
  clearAgentOutput: (id: string) => void;
  requestResume: (id: string) => void;
  consumeResume: (id: string) => void;
  /** Drop a finished agent's dead channel, keeping its record + output so the
   *  sidebar "已结束" group can reopen (resume) it. */
  dropAgentChannel: (id: string) => void;
  setMasterAgentId: (id: string | null) => void;
  setRuntimes: (runtimes: RuntimeInfo[]) => void;
  setWorkerInfos: (infos: WorkerInfo[]) => void;
  upsertWorkerInfo: (info: WorkerInfo) => void;
  addReport: (report: WorkerReport) => void;
  setSmartReturn: (enabled: boolean) => void;
  addTab: (tab: Tab) => void;
  /** Add a tab without changing the active tab (used by session restore). */
  addTabSilent: (tab: Tab) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  setSplit: (paneA: string, paneB: string, direction: "row" | "column") => void;
  clearSplit: () => void;
  removeSplitPane: (id: string) => void;
  setSplitRatio: (ratio: number) => void;
  setDraggedTabId: (id: string | null) => void;
  setWorkerUnlock: (id: string | null) => void;
  setComposerTarget: (target: "agent" | "master") => void;
  toggleComposer: () => void;
  setPermissionMode: (mode: PermissionMode) => void;
  setSpeed: (speed: Speed) => void;
  setSelectedModel: (model: string | null) => void;
  toggleWorkerMode: () => void;
  pushDraft: (text: string) => void;
  navigateDraft: (dir: -1 | 1) => string | null;
  toggleLeftSidebar: () => void;
  toggleRightSidebar: () => void;
  setLeftWidth: (width: number) => void;
  setRightWidth: (width: number) => void;
  setComposerH: (height: number | null) => void;
  setMasterReportH: (height: number) => void;
  setProjects: (projects: string[]) => void;
  setProjectRoots: (roots: Record<string, string>) => void;
  setFocusedProject: (name: string | null) => void;
  projectRoot: (name: string) => string | undefined;
  addProject: (name: string, root?: string) => void;
  removeProject: (name: string) => void;
  /** Move `name` to `targetName`'s position in the sidebar project list
   *  (drag-to-reorder). The pinned master group is never in `projects`. */
  moveProject: (name: string, targetName: string) => void;
  /** Sleep a project: kill all its agent processes + close its tabs/panels to
   *  free CPU/memory. Sessions stay in the DB, so reopening a terminal resumes. */
  sleepProject: (name: string) => void;
  renameProject: (oldName: string, newName: string) => Promise<string>;
  termTemplates: TermTemplate[];
  addTermTemplate: (t: TermTemplate) => void;
  updateTermTemplate: (id: string, patch: Partial<Pick<TermTemplate, "name" | "command">>) => void;
  removeTermTemplate: (id: string) => void;
  setEspStatus: (status: Partial<EspStatus>) => void;
  setEspConnecting: (connecting: boolean) => void;
  applyResourceSample: (resources: AgentResource[]) => void;
  setResourceHistory: (agentId: string, points: ResourcePoint[]) => void;
  setOnboarded: (onboarded: boolean) => void;
  setFontScale: (scale: FontScale) => void;
}

/** Persisted preference: has the user completed first-run onboarding? */
const ONBOARDED_KEY = "capilot.onboarded";
function loadOnboarded(): boolean {
  try {
    return localStorage.getItem(ONBOARDED_KEY) === "1";
  } catch {
    return false;
  }
}

/** Persisted UI font size preset. Fallback: the smallest ("s"). */
const FONT_SCALE_KEY = "capilot.fontScale";
const FONT_SCALES: FontScale[] = ["s", "m", "l", "xl", "xxl"];
function loadFontScale(): FontScale {
  try {
    const v = localStorage.getItem(FONT_SCALE_KEY);
    if (v && (FONT_SCALES as string[]).includes(v)) return v as FontScale;
  } catch {
    // storage unavailable — use base
  }
  return "s";
}

// ── New-terminal templates ──────────────────────────────────────
// The project "+" button opens a picker: bash (fixed, always first) / claude /
// user-defined quick-start commands. Custom templates persist to localStorage.

/** A new-terminal template shown in the project "+" picker. `command` is run
 *  after the shell starts (bash / bash-rc) / ignored for claude; `fixed` (bash)
 *  can't be renamed or removed. */
export interface TermTemplate {
  id: string;
  name: string;
  command: string;
  runtime: "bash" | "bash-rc" | "claude";
  fixed?: boolean;
}

const TERM_TEMPLATES_KEY = "capilot.termTemplates";
const DEFAULT_TEMPLATES: TermTemplate[] = [
  { id: "bash-rc", name: "bash", command: "", runtime: "bash-rc", fixed: true },
  { id: "claude", name: "claude", command: "", runtime: "claude" },
];
function loadTermTemplates(): TermTemplate[] {
  try {
    const raw = localStorage.getItem(TERM_TEMPLATES_KEY);
    const stored: TermTemplate[] = raw ? (JSON.parse(raw) as TermTemplate[]) : [];
    // Drop the old minimal `--norc` "bash" template (superseded by the full
    // bash), and re-label the old "正常 bash" default to just "bash".
    const list = stored.filter((t) => t.id !== "bash");
    for (const t of list) {
      if (t.id === "bash-rc" && t.name === "正常 bash") t.name = "bash";
    }
    const ids = new Set(list.map((t) => t.id));
    for (const b of DEFAULT_TEMPLATES) {
      if (!ids.has(b.id)) list.push(b);
    }
    // Fixed templates (bash) always come first.
    return list.sort(
      (a, b) => Number(b.fixed ?? false) - Number(a.fixed ?? false)
    );
  } catch {
    return DEFAULT_TEMPLATES;
  }
}
function saveTermTemplates(list: TermTemplate[]) {
  try {
    localStorage.setItem(TERM_TEMPLATES_KEY, JSON.stringify(list));
  } catch {
    // ignore storage errors
  }
}

export const useStore = create<AppState>((set, get) => ({
  agents: new Map(),
  agentChannels: new Map(),
  agentOutputs: new Map(),
  masterAgentId: null,
  resumeOnOpen: new Set(),
  closedAgentIds: new Set(),
  runtimes: [],
  workerInfos: [],
  reports: [],
  smartReturn: true,
  tabs: [],
  activeTabId: null,
  composerTarget: "master",
  composerOpen: true,
  permissionMode: "ask",
  speed: "auto",
  selectedModel: null,
  workerMode: false,
  draftHistory: [],
  draftIndex: -1,
  leftSidebarOpen: true,
  rightSidebarOpen: true,
  leftWidth: 248,
  rightWidth: 340,
  composerH: null,
  masterReportH: 0,
  splitPaneA: null,
  splitPaneB: null,
  splitDirection: null,
  splitRatio: 0.5,
  draggedTabId: null,
  workerUnlockId: null,
  espStatus: {
    connected: false,
    kind: null,
    name: null,
    address: null,
    rssi: null,
    battery_pct: null,
    battery_mv: null,
    last_seen_ms: null,
  },
  espConnecting: false,
  agentResources: new Map(),
  resourceHistory: new Map(),
  projects: [],
  projectRoots: {},
  focusedProject: null,
  onboarded: loadOnboarded(),
  termTemplates: loadTermTemplates(),
  fontScale: loadFontScale(),

  addAgent: (info, channel, createdAtTs) =>
    set((s) => {
      // Dead-session guard (close/resume race): an in-flight `agent_resume` can
      // resolve AFTER the user closed & deleted the session. `removeAgent` put
      // the id in `closedAgentIds`; skip the re-add or a zombie agent (status
      // running, dead channel, no on_exit ever arriving) would reappear and be
      // unclosable.
      if (s.closedAgentIds.has(info.id)) return {};
      const agents = new Map(s.agents);
      // Anchor the count-up to a real timestamp: fresh spawns get now; restored
      // sessions carry the DB `created_at` via `createdAtTs`. An existing agent
      // (role-only update) keeps its original `createdAt`.
      const created =
        info.createdAt ??
        (agents.has(info.id) ? agents.get(info.id)!.createdAt : undefined) ??
        (createdAtTs !== undefined ? createdAtTs : Date.now());
      const previous = agents.get(info.id);
      agents.set(info.id, {
        ...info,
        project: info.project ?? previous?.project,
        workspace_id: info.workspace_id ?? previous?.workspace_id,
        createdAt: created,
      });
      const channels = new Map(s.agentChannels);
      // Only overwrite the PTY channel when a new one is supplied. `channel ===
      // null` means a role-only update (setAgentRole) or a restored session with
      // no live PTY yet — in that case preserve the agent's existing live
      // channel so XTermPanel doesn't lose it and fall into the resume path.
      if (channel) channels.set(info.id, channel);
      return { agents, agentChannels: channels };
    }),

  removeAgent: (id) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.delete(id);
      const channels = new Map(s.agentChannels);
      channels.delete(id);
      const outputs = new Map(s.agentOutputs);
      outputs.delete(id);
      const resources = new Map(s.agentResources);
      resources.delete(id);
      const history = new Map(s.resourceHistory);
      history.delete(id);
      const resumeOnOpen = new Set(s.resumeOnOpen);
      resumeOnOpen.delete(id);
      const closedAgentIds = new Set(s.closedAgentIds);
      closedAgentIds.add(id);
      // If the removed agent was the master, clear masterAgentId so the composer
      // doesn't see a stale master and spawn a duplicate (Bug 4).
      return {
        agents,
        agentChannels: channels,
        agentOutputs: outputs,
        agentResources: resources,
        resourceHistory: history,
        resumeOnOpen,
        closedAgentIds,
        ...(s.masterAgentId === id ? { masterAgentId: null } : {}),
      };
    }),

  updateAgentStatus: (id, status) =>
    set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, status });
      return { agents };
    }),

  updateAgentAttention: (id, reason) =>
    set((s) => {
      const agents = new Map(s.agents);
      const agent = agents.get(id);
      if (agent) {
        agents.set(id, {
          ...agent,
          requires_attention: reason !== null,
          attention_reason: reason,
        });
      }
      return { agents };
    }),

  appendAgentOutput: (id, data) =>
    set((s) => {
      // A closed/deleted session accepts no more output — a stale resume
      // channel must not resurrect its buffer.
      if (s.closedAgentIds.has(id)) return {};
      const outputs = new Map(s.agentOutputs);
      const prev = outputs.get(id) || [];
      // Hot path (every PTY chunk): append in place instead of rebuilding the
      // whole array — a per-chunk `[...prev, ...data]` copy is O(n²) on
      // sustained output. Plain loop (vs push(...data)) also avoids the
      // spread-argument limit for very large chunks.
      for (let i = 0; i < data.length; i++) prev.push(data[i]);
      // Slide a window over the OLDEST bytes when the cap is hit — the old code
      // discarded the ENTIRE history for the last chunk, zeroing the scrollback.
      // (Each element is one byte, so length === byte count.)
      if (prev.length > MAX_OUTPUT_BUFFER) {
        prev.splice(0, prev.length - MAX_OUTPUT_BUFFER);
      }
      outputs.set(id, prev);
      return { agentOutputs: outputs };
    }),

  clearAgentOutput: (id) =>
    set((s) => {
      const outputs = new Map(s.agentOutputs);
      outputs.delete(id);
      return { agentOutputs: outputs };
    }),

  requestResume: (id) =>
    set((s) => ({ resumeOnOpen: new Set(s.resumeOnOpen).add(id) })),

  consumeResume: (id) =>
    set((s) => {
      if (!s.resumeOnOpen.has(id)) return {};
      const resumeOnOpen = new Set(s.resumeOnOpen);
      resumeOnOpen.delete(id);
      return { resumeOnOpen };
    }),

  dropAgentChannel: (id) =>
    set((s) => {
      if (!s.agentChannels.has(id)) return {};
      const agentChannels = new Map(s.agentChannels);
      agentChannels.delete(id);
      return { agentChannels };
    }),

  setMasterAgentId: (id) => set({ masterAgentId: id }),

  setRuntimes: (runtimes) => set({ runtimes }),

  setWorkerInfos: (infos) => set({ workerInfos: infos }),

  upsertWorkerInfo: (info) =>
    set((s) => {
      const infos = [...s.workerInfos];
      const idx = infos.findIndex((w) => w.id === info.id);
      if (idx >= 0) infos[idx] = info;
      else infos.push(info);
      return { workerInfos: infos };
    }),

  addReport: (report) =>
    set((s) => ({
      reports: [report, ...s.reports].slice(0, 50),
    })),

  setSmartReturn: (enabled) => set({ smartReturn: enabled }),

  addTab: (tab) =>
    set((s) => {
      const tabs = [...s.tabs.filter((t) => t.id !== tab.id), tab];
      // With an active split, surface a newly opened tab in the primary pane.
      const splitActive = s.splitPaneA !== null && s.splitPaneB !== null;
      if (splitActive && s.splitPaneA !== tab.id && s.splitPaneB !== tab.id) {
        return { tabs, activeTabId: tab.id, splitPaneA: tab.id };
      }
      return { tabs, activeTabId: tab.id };
    }),

  addTabSilent: (tab) =>
    set((s) => ({
      tabs: [...s.tabs.filter((t) => t.id !== tab.id), tab],
    })),

  closeTab: (id) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== id);
      // Closing a tab that occupies a split pane collapses the split so the
      // remaining pane becomes the single view.
      const splitActive = s.splitPaneA !== null && s.splitPaneB !== null;
      if (splitActive) {
        if (s.splitPaneA === id && s.splitPaneB === id) {
          return {
            tabs,
            activeTabId: tabs[tabs.length - 1]?.id ?? null,
            splitPaneA: null,
            splitPaneB: null,
            splitDirection: null,
          };
        }
        if (s.splitPaneA === id) {
          return {
            tabs,
            activeTabId: s.splitPaneB,
            splitPaneA: null,
            splitPaneB: null,
            splitDirection: null,
          };
        }
        if (s.splitPaneB === id) {
          return {
            tabs,
            activeTabId: s.splitPaneA,
            splitPaneA: null,
            splitPaneB: null,
            splitDirection: null,
          };
        }
      }
      const activeTabId =
        s.activeTabId === id ? (tabs[tabs.length - 1]?.id ?? null) : s.activeTabId;
      return { tabs, activeTabId };
    }),

  setActiveTab: (id) =>
    set((s) => {
      // With an active split, clicking a tab that isn't already visible focuses
      // it in the primary pane (paneA).
      const splitActive = s.splitPaneA !== null && s.splitPaneB !== null;
      if (splitActive && s.splitPaneA !== id && s.splitPaneB !== id) {
        return { activeTabId: id, splitPaneA: id };
      }
      return { activeTabId: id };
    }),

  setSplit: (paneA, paneB, direction) =>
    set({
      splitPaneA: paneA,
      splitPaneB: paneB,
      splitDirection: direction,
      splitRatio: 0.5,
    }),

  clearSplit: () =>
    set({ splitPaneA: null, splitPaneB: null, splitDirection: null }),

  removeSplitPane: (id) =>
    set((s) => {
      if (s.splitPaneA === null || s.splitPaneB === null) return {};
      if (s.splitPaneA === id) {
        return {
          activeTabId: s.splitPaneB,
          splitPaneA: null,
          splitPaneB: null,
          splitDirection: null,
        };
      }
      if (s.splitPaneB === id) {
        return {
          activeTabId: s.splitPaneA,
          splitPaneA: null,
          splitPaneB: null,
          splitDirection: null,
        };
      }
      return {};
    }),

  setSplitRatio: (ratio) =>
    set({ splitRatio: Math.max(0.2, Math.min(0.8, ratio)) }),

  setDraggedTabId: (id) => set({ draggedTabId: id }),

  setWorkerUnlock: (id) => set({ workerUnlockId: id }),

  setComposerTarget: (target) => set({ composerTarget: target }),

  toggleComposer: () => set((s) => ({ composerOpen: !s.composerOpen })),

  setPermissionMode: (mode) => set({ permissionMode: mode }),

  setSpeed: (speed) => set({ speed }),

  setSelectedModel: (model) => set({ selectedModel: model }),

  toggleWorkerMode: () => set((s) => ({ workerMode: !s.workerMode })),

  pushDraft: (text) =>
    set((s) => ({
      draftHistory: [text, ...s.draftHistory.slice(0, 49)],
      draftIndex: -1,
    })),

  navigateDraft: (dir) => {
    const { draftHistory, draftIndex } = get();
    const next = draftIndex + dir;
    if (next < -1 || next >= draftHistory.length) return null;
    set({ draftIndex: next });
    return next === -1 ? "" : draftHistory[next];
  },

  toggleLeftSidebar: () => set((s) => ({ leftSidebarOpen: !s.leftSidebarOpen })),

  toggleRightSidebar: () => set((s) => ({ rightSidebarOpen: !s.rightSidebarOpen })),
  setLeftWidth: (width) => set({ leftWidth: width }),
  setRightWidth: (width) => set({ rightWidth: width }),
  setComposerH: (height) => set({ composerH: height }),
  setMasterReportH: (height) => set({ masterReportH: height }),

  setProjects: (projects) => set({ projects }),

  setProjectRoots: (roots) =>
    set((s) => ({ projectRoots: { ...s.projectRoots, ...roots } })),

  setFocusedProject: (name) => set({ focusedProject: name }),

  projectRoot: (name) => get().projectRoots[name],

  addProject: (name, root) =>
    set((s) => {
      if (s.projects.includes(name)) {
        // Already listed — keep the name list untouched; only (re)record the
        // root mapping when one is provided.
        return root !== undefined
          ? { projectRoots: { ...s.projectRoots, [name]: root } }
          : {};
      }
      return {
        projects: [...s.projects, name],
        ...(root !== undefined
          ? { projectRoots: { ...s.projectRoots, [name]: root } }
          : {}),
      };
    }),

  removeProject: (name) => {
    const s = get();
    // Guard: the Master group is pinned and never deletable. It is not part of
    // `projects` and its agents live under the "master" project group, so a
    // stray call must not kill the master session or its terminals.
    if (name === "master") return;
    // Remove the project from the list and drop its root mapping; clear focus
    // when the removed project was the focused one (tab bar then shows all tabs).
    const projectRoots = { ...s.projectRoots };
    delete projectRoots[name];
    set({
      projects: s.projects.filter((p) => p !== name),
      projectRoots,
      focusedProject: s.focusedProject === name ? null : s.focusedProject,
    });

    // Close + kill every agent whose workspace cwd belongs to the removed
    // project (matches the sidebar's projectOf grouping).
    const doomed: string[] = [];
    s.agents.forEach((a, id) => {
      if (projectOfCwd(a.cwd) === name) doomed.push(id);
    });
    for (const id of doomed) {
      // sessions_delete kills the PTY and drops the DB row so the agent can't
      // resurrect on restart; fall back to a plain kill if cleanup fails.
      invoke("sessions_delete", { id })
        .catch(() => invoke("agent_kill", { id }).catch(() => {}));
      s.closeTab(id);
      s.removeAgent(id);
    }

    // Guard: removing the master's project clears the master slot. removeAgent
    // already clears masterAgentId when the master agent itself is removed;
    // this covers a master whose cwd escaped the match, and drops the pinned
    // "master" placeholder tab so a stale terminal doesn't linger.
    const masterId = s.masterAgentId;
    if (masterId) {
      const master = s.agents.get(masterId);
      if (!master || projectOfCwd(master.cwd) === name) {
        set({ masterAgentId: null });
        if (s.tabs.some((t) => t.id === "master")) s.closeTab("master");
      }
    }

    // Delete the project's workspace dir (sessions / agent metadata / context).
    // Custom-rooted projects only lose this metadata dir — the real folder
    // (picked / cloned) is never touched by the backend command.
    invoke("delete_project", { name }).catch(() => {});
  },

  // Move a project to another project's position in the sidebar list
  // (drag-to-reorder). No-op when either name is missing or they're the same.
  moveProject: (name, targetName) =>
    set((s) => {
      const projects = [...s.projects];
      const from = projects.indexOf(name);
      const to = projects.indexOf(targetName);
      if (from === -1 || to === -1 || from === to) return {};
      projects.splice(from, 1);
      const toIdx = projects.indexOf(targetName);
      projects.splice(toIdx, 0, name);
      return { projects };
    }),

  // Sleep a project (free CPU/memory): kill every agent's PTY process, close its
  // terminal + editor/diff tabs. The agent stays in the store as idle (reopening
  // the terminal resumes the session) and the DB rows persist for restart.
  sleepProject: (name) => {
    const s = get();
    if (name === "master") return;
    const root = s.projectRoots[name];
    const doomed: string[] = [];
    s.agents.forEach((a, id) => {
      if (projectOfCwd(a.cwd) === name) doomed.push(id);
    });
    const channels = new Map(s.agentChannels);
    for (const id of doomed) {
      invoke("agent_kill", { id }).catch(() => {});
      s.closeTab(id);
      channels.delete(id);
    }
    set({ agentChannels: channels });
    for (const id of doomed) s.updateAgentStatus(id, "idle");
    // Close editor / diff tabs whose file lives under the project root.
    if (root) {
      const base = root.endsWith("/") ? root : root + "/";
      for (const t of [...s.tabs]) {
        if (t.filePath && (t.filePath === root || t.filePath.startsWith(base))) {
          s.closeTab(t.id);
        }
      }
    }
  },

  addTermTemplate: (t) =>
    set((s) => {
      const list = [...s.termTemplates, t];
      saveTermTemplates(list);
      return { termTemplates: list };
    }),

  updateTermTemplate: (id, patch) =>
    set((s) => {
      const list = s.termTemplates.map((t) =>
        t.id === id ? { ...t, ...patch } : t
      );
      saveTermTemplates(list);
      return { termTemplates: list };
    }),

  removeTermTemplate: (id) =>
    set((s) => {
      // The fixed bash template can't be removed.
      const list = s.termTemplates.filter((t) => t.id !== id && !t.fixed);
      saveTermTemplates(list);
      return { termTemplates: list };
    }),

  // Rename a workspace project: the backend renames the `workspaces/<old>` dir
  // (and rewrites sessions.db + agent-meta cwds). Here we keep the store in
  // sync — re-key projectRoots with the returned root, replace the name in the
  // list, move focus, and rewrite agent cwds that pointed into the old dir.
  renameProject: async (oldName, newName) => {
    const s = get();
    const newRoot = await invoke<string>("rename_project", {
      old: oldName,
      new: newName,
    });
    const projectRoots = { ...s.projectRoots };
    delete projectRoots[oldName];
    projectRoots[newName] = newRoot;
    const projects = s.projects.map((p) => (p === oldName ? newName : p));
    const focusedProject = s.focusedProject === oldName ? newName : s.focusedProject;
    const agents = new Map(s.agents);
    agents.forEach((a, id) => {
      // Rewrite agent cwds pointing into the old workspace dir. The boundary
      // check (old name followed by `/` or end) keeps `foo` from corrupting a
      // sibling project whose name merely starts with `foo` (e.g. `foobar`).
      const marker = `workspaces/${oldName}`;
      const idx = a.cwd.indexOf(marker);
      if (idx !== -1) {
        const after = a.cwd.slice(idx + marker.length);
        if (after === "" || after.startsWith("/")) {
          agents.set(id, {
            ...a,
            cwd: a.cwd.slice(0, idx) + `workspaces/${newName}` + after,
          });
        }
      }
    });
    set({ projects, projectRoots, focusedProject, agents });
    return newName;
  },

  setEspStatus: (status) =>
    set((s) => ({ espStatus: { ...s.espStatus, ...status } })),

  setEspConnecting: (connecting) => set({ espConnecting: connecting }),

  applyResourceSample: (resources) =>
    set((s) => {
      if (resources.length === 0) return {};
      const agentResources = new Map(s.agentResources);
      const resourceHistory = new Map(s.resourceHistory);
      for (const r of resources) {
        const point: ResourcePoint = { cpu_pct: r.cpu_pct, mem_bytes: r.mem_bytes };
        agentResources.set(r.agent_id, point);
        const prev = resourceHistory.get(r.agent_id) || [];
        resourceHistory.set(r.agent_id, [...prev.slice(-59), point]);
      }
      return { agentResources, resourceHistory };
    }),

  setResourceHistory: (agentId, points) =>
    set((s) => {
      const resourceHistory = new Map(s.resourceHistory);
      resourceHistory.set(agentId, points.slice(-60));
      return { resourceHistory };
    }),

  setOnboarded: (onboarded) => {
    try {
      localStorage.setItem(ONBOARDED_KEY, onboarded ? "1" : "0");
    } catch {
      // ignore storage errors
    }
    set({ onboarded });
  },

  setFontScale: (scale) => {
    try {
      localStorage.setItem(FONT_SCALE_KEY, scale);
    } catch {
      // ignore storage errors
    }
    set({ fontScale: scale });
  },
}));

// ── Buffered-output accessors ───────────────────────────────────
// `agentOutputs` lives in the store (kept reactive so a mounting XTermPanel can
// drain it via getState()). These thin wrappers give readers a stable entry
// point that doesn't reach into the store shape directly.

/** Read an agent's buffered terminal output (bytes buffered before/while no
 *  XTermPanel is attached). Returns undefined when nothing is buffered. */
export function getAgentOutput(id: string): number[] | undefined {
  return useStore.getState().agentOutputs.get(id);
}

/** Drop an agent's buffered terminal output. */
export function clearAgentOutput(id: string): void {
  useStore.getState().clearAgentOutput(id);
}
