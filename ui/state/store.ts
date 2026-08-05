import { create } from "zustand";
import type { Channel } from "@tauri-apps/api/core";

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
}

export interface Tab {
  id: string;
  type: "agent" | "editor";
  agentId?: string;
  filePath?: string;
  title: string;
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

// ── Store ────────────────────────────────────────────────────────

interface AppState {
  // Agents
  agents: Map<string, AgentInfo>;
  agentChannels: Map<string, Channel<number[]>>;

  // Runtimes
  runtimes: RuntimeInfo[];

  // UI tabs
  tabs: Tab[];
  activeTabId: string | null;

  // Composer
  composerTarget: "agent" | "master";
  composerOpen: boolean;
  permissionMode: PermissionMode;
  speed: Speed;
  draftHistory: string[];
  draftIndex: number;

  // Sidebars
  leftSidebarOpen: boolean;
  rightSidebarOpen: boolean;

  // ESP
  espStatus: EspStatus;
  espConnecting: boolean;

  // Actions
  addAgent: (info: AgentInfo, channel: Channel<number[]>) => void;
  removeAgent: (id: string) => void;
  updateAgentStatus: (id: string, status: AgentStatus) => void;
  setRuntimes: (runtimes: RuntimeInfo[]) => void;
  addTab: (tab: Tab) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  setComposerTarget: (target: "agent" | "master") => void;
  toggleComposer: () => void;
  setPermissionMode: (mode: PermissionMode) => void;
  setSpeed: (speed: Speed) => void;
  pushDraft: (text: string) => void;
  navigateDraft: (dir: -1 | 1) => string | null;
  toggleLeftSidebar: () => void;
  setEspStatus: (status: Partial<EspStatus>) => void;
  setEspConnecting: (connecting: boolean) => void;
}

export const useStore = create<AppState>((set, get) => ({
  agents: new Map(),
  agentChannels: new Map(),
  runtimes: [],
  tabs: [],
  activeTabId: null,
  composerTarget: "master",
  composerOpen: true,
  permissionMode: "ask",
  speed: "auto",
  draftHistory: [],
  draftIndex: -1,
  leftSidebarOpen: true,
  rightSidebarOpen: true,
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
      channels.set(info.id, channel);
      return { agents, agentChannels: channels };
    }),

  removeAgent: (id) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.delete(id);
      const channels = new Map(s.agentChannels);
      channels.delete(id);
      return { agents, agentChannels: channels };
    }),

  updateAgentStatus: (id, status) =>
    set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, status });
      return { agents };
    }),

  setRuntimes: (runtimes) => set({ runtimes }),

  addTab: (tab) =>
    set((s) => ({
      tabs: [...s.tabs.filter((t) => t.id !== tab.id), tab],
      activeTabId: tab.id,
    })),

  closeTab: (id) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== id);
      const activeTabId =
        s.activeTabId === id ? (tabs[tabs.length - 1]?.id ?? null) : s.activeTabId;
      return { tabs, activeTabId };
    }),

  setActiveTab: (id) => set({ activeTabId: id }),

  setComposerTarget: (target) => set({ composerTarget: target }),

  toggleComposer: () => set((s) => ({ composerOpen: !s.composerOpen })),

  setPermissionMode: (mode) => set({ permissionMode: mode }),

  setSpeed: (speed) => set({ speed }),

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

  setEspStatus: (status) =>
    set((s) => ({ espStatus: { ...s.espStatus, ...status } })),

  setEspConnecting: (connecting) => set({ espConnecting: connecting }),
}));
