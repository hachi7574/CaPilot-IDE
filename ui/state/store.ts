import { create } from "zustand";
import { Channel } from "@tauri-apps/api/core";

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

export interface AgentInfo {
  id: string;
  runtime: string;
  role: AgentRole;
  status: AgentStatus;
  title: string;
  cwd: string;
  pid: number | null;
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
  type: "agent" | "editor";
  agentId?: string;
  filePath?: string;
  title: string;
}

/** Matches the Rust `AgentSessionRecord` (snake_case keys). */
export interface RestoredSession {
  id: string;
  project: string;
  role: AgentRole;
  runtime: string;
  resume_key: string | null;
  cwd: string;
  title: string;
  status: string;
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

// ── Helpers ─────────────────────────────────────────────────────

/** Max buffered bytes per agent before XTermPanel attaches. */
const MAX_OUTPUT_BUFFER = 2_000_000;

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

// ── Store ────────────────────────────────────────────────────────

interface AppState {
  // Agents
  agents: Map<string, AgentInfo>;
  agentChannels: Map<string, Channel<number[]>>;
  /** Output buffered before a terminal attached (and between mounts). */
  agentOutputs: Map<string, number[]>;
  masterAgentId: string | null;

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

  // Sidebars
  leftSidebarOpen: boolean;
  rightSidebarOpen: boolean;
  leftWidth: number;
  rightWidth: number;

  // ESP
  espStatus: EspStatus;
  espConnecting: boolean;

  // Actions
  addAgent: (info: AgentInfo, channel: Channel<number[]> | null) => void;
  removeAgent: (id: string) => void;
  updateAgentStatus: (id: string, status: AgentStatus) => void;
  appendAgentOutput: (id: string, data: number[]) => void;
  clearAgentOutput: (id: string) => void;
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
  setEspStatus: (status: Partial<EspStatus>) => void;
  setEspConnecting: (connecting: boolean) => void;
}

export const useStore = create<AppState>((set, get) => ({
  agents: new Map(),
  agentChannels: new Map(),
  agentOutputs: new Map(),
  masterAgentId: null,
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

  addAgent: (info, channel) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.set(info.id, info);
      const channels = new Map(s.agentChannels);
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
      return { agents, agentChannels: channels, agentOutputs: outputs };
    }),

  updateAgentStatus: (id, status) =>
    set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, status });
      return { agents };
    }),

  appendAgentOutput: (id, data) =>
    set((s) => {
      const outputs = new Map(s.agentOutputs);
      const prev = outputs.get(id) || [];
      const next = prev.length + data.length > MAX_OUTPUT_BUFFER ? data : [...prev, ...data];
      outputs.set(id, next);
      return { agentOutputs: outputs };
    }),

  clearAgentOutput: (id) =>
    set((s) => {
      const outputs = new Map(s.agentOutputs);
      outputs.delete(id);
      return { agentOutputs: outputs };
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

  setEspStatus: (status) =>
    set((s) => ({ espStatus: { ...s.espStatus, ...status } })),

  setEspConnecting: (connecting) => set({ espConnecting: connecting }),
}));
