// ── Structured Agent Runtime (architecture §13/§14) ───────────────
//
// Frontend mirror of the daemon's `AgentManager` wire types (serde JSON), plus
// the Zustand slice that owns their live state. A structured agent is distinct
// from a legacy PTY terminal: it renders a canonical timeline (messages / tool
// calls / plans / errors), surfaces permission requests inline, and is driven by
// `agent_start_turn` rather than raw PTY writes.
//
// Live updates arrive as sequenced `agent://agent-event` emissions (`{ id, seq,
// event }`). Reconnect discipline: apply only `seq > view.lastSeq`; events that
// arrive before a snapshot (reconnect gap) are buffered and replayed once the
// snapshot lands. Snapshot fetch = `agent_snapshot` (returns `AgentSnapshot`
// with `last_seq`, the daemon's high-water mark).

import { useEffect } from "react";
import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";

// ── Wire types (mirror src/agent_provider/types.rs + manager.rs serde JSON) ──

/** `ProviderCapabilities` — `#[serde(rename_all = "camelCase")]`. */
export interface ProviderCapabilities {
  sessionResume: boolean;
  sessionList: boolean;
  structuredTools: boolean;
  reasoningStream: boolean;
  permissions: boolean;
  configOptions: boolean;
  slashCommands: boolean;
  mcpServers: boolean;
  images: boolean;
  contextUsage: boolean;
}

export interface ModelDefinition {
  id: string;
  label: string;
  context_window?: number | null;
  reasoning_efforts: string[];
  is_default: boolean;
}

export interface SelectOption {
  id: string;
  label: string;
}

/** `ConfigOption` — `#[serde(tag = "type", rename_all = "snake_case")]`,
 *  flattened fields (no content wrapper). */
export type ConfigOption =
  | {
      type: "select";
      id: string;
      label: string;
      category?: string | null;
      current: string;
      options: SelectOption[];
    }
  | {
      type: "boolean";
      id: string;
      label: string;
      category?: string | null;
      current: boolean;
    };

/** `ConfigValue` — untagged String | Bool. */
export type ConfigValue = string | boolean;

export interface ProviderCatalog {
  models: ModelDefinition[];
  config_options: ConfigOption[];
  capabilities: ProviderCapabilities;
}

export interface ProviderDiagnostic {
  available: boolean;
  authenticated: boolean;
  version?: string | null;
  message?: string | null;
}

/** One registered provider from `agent_provider_list`. `backend_kind` is the
 *  daemon's authority on the backend type (`"acp"` | `"direct"`) — the
 *  frontend creates agents with the provider's real kind, never a hardcoded
 *  value (handoff §6 / §18.2). */
export interface ProviderInfo {
  provider_id: string;
  backend_kind: string;
}

/** `ContextUsage` — `#[serde(rename_all = "camelCase")]`. */
export interface ContextUsage {
  contextWindowUsedTokens: number | null;
  contextWindowMaxTokens: number | null;
}

export type MessageRole = "user" | "assistant";

export interface MessageItem {
  item_id: string;
  role: MessageRole;
  text: string;
  created_at: number;
  metadata?: unknown | null;
}

export type ToolCallStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export interface ToolCallItem {
  item_id: string;
  tool_name: string;
  tool_input?: unknown | null;
  tool_output?: unknown | null;
  status: ToolCallStatus;
  created_at: number;
  metadata?: unknown | null;
}

export interface PlanItem {
  item_id: string;
  title: string;
  content: string;
  created_at: number;
}

export interface ErrorItem {
  item_id: string;
  message: string;
  created_at: number;
}

/** `TimelineItem` — `#[serde(tag = "kind", content = "data")]`. */
export type TimelineItem =
  | { kind: "user_message"; data: MessageItem }
  | { kind: "assistant_message"; data: MessageItem }
  | { kind: "reasoning"; data: MessageItem }
  | { kind: "tool_call"; data: ToolCallItem }
  | { kind: "plan"; data: PlanItem }
  | { kind: "error"; data: ErrorItem };

/** Stable item id shared across streaming updates (op `started`/`appended`/
 *  `replaced`/`finished` reuse the same id). */
export function timelineItemId(item: TimelineItem): string {
  return item.data.item_id;
}

export type ItemStatus = "pending" | "complete" | "failed" | "cancelled";

/** `TimelineEvent` — `#[serde(tag = "op", rename_all = "snake_case")]`. */
export type TimelineEvent =
  | { op: "started"; item: TimelineItem }
  | { op: "appended"; item_id: string; text_delta: string }
  | { op: "replaced"; item: TimelineItem }
  | { op: "finished"; item_id: string; status: ItemStatus };

export type PermissionKind =
  | "tool_call"
  | "terminal_command"
  | "file_change"
  | "mode_change"
  | "question"
  | "other";

export type PermissionBehavior = "allow" | "deny" | "ask" | "escalate";

export interface PermissionSubject {
  kind: PermissionKind;
  title: string;
  description?: string | null;
  icon?: string | null;
}

export interface PermissionAction {
  id: string;
  label: string;
  behavior: PermissionBehavior;
}

export interface PermissionRequest {
  id: string;
  agent_id: string;
  kind: PermissionKind;
  title: string;
  description?: string | null;
  subject: PermissionSubject;
  actions: PermissionAction[];
}

export interface PermissionResolution {
  request_id: string;
  action_id: string;
  resolved_at: number;
}

/** `AgentStatus` — `#[serde(rename_all = "snake_case")]`. */
export type AgentStatus =
  | "initializing"
  | "idle"
  | "running"
  | "waiting_permission"
  | "waiting_input"
  | "error"
  | "closed";

export interface PersistenceHandle {
  provider_id: string;
  runtime_session_id: string;
  native_handle?: unknown | null;
  metadata?: unknown | null;
}

export interface SessionReady {
  provider_id: string;
  runtime_session_id?: string | null;
  capabilities: ProviderCapabilities;
  persistence?: PersistenceHandle | null;
}

export interface TurnStarted {
  turn_id: string;
  client_message_id: string;
}

export interface TurnCompleted {
  turn_id: string;
}

export interface TurnCancelled {
  turn_id: string;
}

export interface TurnFailed {
  turn_id: string;
  message: string;
}

/** `AgentEvent` — `#[serde(tag = "event", content = "data")]`. */
export type AgentEvent =
  | { event: "session_ready"; data: SessionReady }
  | { event: "turn_started"; data: TurnStarted }
  | { event: "timeline"; data: TimelineEvent }
  | { event: "permission_requested"; data: PermissionRequest }
  | { event: "permission_resolved"; data: PermissionResolution }
  | { event: "config_updated"; data: ConfigOption[] }
  | { event: "context_usage_updated"; data: ContextUsage }
  | { event: "turn_completed"; data: TurnCompleted }
  | { event: "turn_cancelled"; data: TurnCancelled }
  | { event: "turn_failed"; data: TurnFailed }
  | { event: "session_closed" };

export interface AgentRecord {
  agent_id: string;
  provider_id: string;
  backend_kind: string;
  workspace_id?: string | null;
  cwd: string;
  status: AgentStatus;
  config: [string, ConfigValue][];
  capabilities: ProviderCapabilities;
  persistence?: PersistenceHandle | null;
  last_event_seq: number;
  created_at: number;
  updated_at: number;
}

export interface AgentSnapshot {
  agent: AgentRecord;
  timeline: TimelineItem[];
  pending_permissions: PermissionRequest[];
  last_seq: number;
}

/** Live view of one structured agent: the latest snapshot plus the frontend's
 *  applied-event watermark (`lastSeq`). `usage` is the optional context-window
 *  sample pushed by `context_usage_updated`. */
export interface StructuredAgentView {
  snapshot: AgentSnapshot;
  lastSeq: number;
  usage?: ContextUsage | null;
}

/** Current value of an agent config key (e.g. `"model"`), from the record's
 *  `[key, value]` config tuple list. */
export function agentConfigValue(
  agent: AgentRecord,
  key: string
): ConfigValue | undefined {
  return agent.config.find(([k]) => k === key)?.[1];
}

// ── Store ────────────────────────────────────────────────────────

interface StructuredAgentState {
  agents: Map<string, StructuredAgentView>;
  /** provider_id → catalog (models + config options), fetched lazily. */
  catalogs: Record<string, ProviderCatalog>;
  /** provider_id → diagnostic probe result. */
  diagnostics: Record<string, ProviderDiagnostic>;
  /** Events that arrived before the snapshot (reconnect gap), keyed by agent_id.
   *  Replayed by `setSnapshot` with `seq > snapshot.last_seq`. */
  pendingEvents: Map<string, { seq: number; event: AgentEvent }[]>;

  setSnapshot: (agentId: string, snapshot: AgentSnapshot) => void;
  applyEvent: (agentId: string, seq: number, event: AgentEvent) => void;
  removeAgent: (agentId: string) => void;
  setCatalog: (providerId: string, catalog: ProviderCatalog) => void;
  setDiagnostic: (providerId: string, diag: ProviderDiagnostic) => void;
}

/** English → 中文 status label for structured agent tabs/panel. */
export const AGENT_STATUS_TEXT: Record<AgentStatus, string> = {
  initializing: "初始化",
  idle: "空闲",
  running: "运行中",
  waiting_permission: "待授权",
  waiting_input: "待输入",
  error: "出错",
  closed: "已关闭",
};

// ── Immutable timeline helpers (upsert by item id, never by text) ──

function upsertTimeline(timeline: TimelineItem[], item: TimelineItem): TimelineItem[] {
  const id = timelineItemId(item);
  const idx = timeline.findIndex((it) => timelineItemId(it) === id);
  if (idx === -1) return [...timeline, item];
  const next = timeline.slice();
  next[idx] = item;
  return next;
}

function appendText(
  timeline: TimelineItem[],
  itemId: string,
  delta: string
): TimelineItem[] {
  const idx = timeline.findIndex((it) => timelineItemId(it) === itemId);
  if (idx === -1) return timeline;
  const it = timeline[idx];
  // Only text-bearing items stream deltas.
  if (it.kind !== "user_message" && it.kind !== "assistant_message" && it.kind !== "reasoning") {
    return timeline;
  }
  const next = timeline.slice();
  next[idx] = { ...it, data: { ...it.data, text: it.data.text + delta } };
  return next;
}

function finishTimeline(
  timeline: TimelineItem[],
  itemId: string,
  status: ItemStatus
): TimelineItem[] {
  const idx = timeline.findIndex((it) => timelineItemId(it) === itemId);
  if (idx === -1) return timeline;
  const it = timeline[idx];
  if (it.kind !== "tool_call") return timeline;
  const next = timeline.slice();
  const toolStatus: ToolCallStatus =
    status === "complete"
      ? "completed"
      : status === "failed"
        ? "failed"
        : status === "cancelled"
          ? "cancelled"
          : it.data.status;
  next[idx] = { ...it, data: { ...it.data, status: toolStatus } };
  return next;
}

// ── Store ────────────────────────────────────────────────────────

export const useStructuredStore = create<StructuredAgentState>((set) => ({
  agents: new Map(),
  catalogs: {},
  diagnostics: {},
  pendingEvents: new Map(),

  setSnapshot: (agentId, snapshot) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.set(agentId, { snapshot, lastSeq: snapshot.last_seq });
      // Drain the reconnect gap: events that arrived before this snapshot, applied
      // only above the daemon's high-water mark.
      const buffered = s.pendingEvents.get(agentId);
      const pendingEvents = new Map(s.pendingEvents);
      if (buffered && buffered.length) {
        let view = agents.get(agentId)!;
        for (const { seq, event } of buffered) {
          if (seq <= view.lastSeq) continue;
          view = reduceEvent(view, event);
          view = { ...view, lastSeq: seq };
        }
        agents.set(agentId, view);
        pendingEvents.delete(agentId);
      }
      return { agents, pendingEvents };
    }),

  applyEvent: (agentId, seq, event) =>
    set((s) => {
      const view = s.agents.get(agentId);
      if (!view) {
        // Reconnect gap: no snapshot yet. Buffer the event for replay.
        const pendingEvents = new Map(s.pendingEvents);
        const list = pendingEvents.get(agentId) ?? [];
        list.push({ seq, event });
        pendingEvents.set(agentId, list);
        return { pendingEvents };
      }
      if (seq <= view.lastSeq) return {}; // stale replay — already applied
      const agents = new Map(s.agents);
      agents.set(agentId, { ...reduceEvent(view, event), lastSeq: seq });
      return { agents };
    }),

  removeAgent: (agentId) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.delete(agentId);
      const pendingEvents = new Map(s.pendingEvents);
      pendingEvents.delete(agentId);
      return { agents, pendingEvents };
    }),

  setCatalog: (providerId, catalog) =>
    set((s) => ({ catalogs: { ...s.catalogs, [providerId]: catalog } })),

  setDiagnostic: (providerId, diag) =>
    set((s) => ({ diagnostics: { ...s.diagnostics, [providerId]: diag } })),
}));

/** Pure reducer: apply one structured event to a view (immutable). */
function reduceEvent(view: StructuredAgentView, event: AgentEvent): StructuredAgentView {
  const snap = view.snapshot;
  const agent = snap.agent;
  switch (event.event) {
    case "session_ready": {
      const { capabilities, persistence } = event.data;
      const status: AgentStatus = agent.status === "initializing" ? "idle" : agent.status;
      return {
        ...view,
        snapshot: {
          ...snap,
          agent: { ...agent, status, capabilities, persistence },
        },
      };
    }
    case "turn_started":
      return {
        ...view,
        snapshot: { ...snap, agent: { ...agent, status: "running" } },
      };
    case "timeline": {
      const op = event.data;
      let timeline = snap.timeline;
      switch (op.op) {
        case "started":
          timeline = upsertTimeline(timeline, op.item);
          break;
        case "appended":
          timeline = appendText(timeline, op.item_id, op.text_delta);
          break;
        case "replaced":
          timeline = upsertTimeline(timeline, op.item);
          break;
        case "finished":
          timeline = finishTimeline(timeline, op.item_id, op.status);
          break;
      }
      return { ...view, snapshot: { ...snap, timeline } };
    }
    case "permission_requested": {
      const req = event.data;
      const existing = snap.pending_permissions.some((p) => p.id === req.id);
      const pending_permissions = existing
        ? snap.pending_permissions
        : [...snap.pending_permissions, req];
      return {
        ...view,
        snapshot: {
          ...snap,
          agent: { ...agent, status: "waiting_permission" },
          pending_permissions,
        },
      };
    }
    case "permission_resolved": {
      const { request_id } = event.data;
      return {
        ...view,
        snapshot: {
          ...snap,
          pending_permissions: snap.pending_permissions.filter(
            (p) => p.id !== request_id
          ),
        },
      };
    }
    case "config_updated":
      // The event carries the provider's current option list; fold it into the
      // shared catalog so the config selector stays in sync.
      applyCatalogOptions(agent.provider_id, event.data);
      return view;
    case "context_usage_updated":
      return { ...view, usage: event.data };
    case "turn_completed":
    case "turn_cancelled":
      return {
        ...view,
        snapshot: {
          ...snap,
          agent: {
            ...agent,
            status: agent.status === "waiting_permission" ? agent.status : "idle",
          },
        },
      };
    case "turn_failed":
      return {
        ...view,
        snapshot: { ...snap, agent: { ...agent, status: "error" } },
      };
    case "session_closed":
      return {
        ...view,
        snapshot: { ...snap, agent: { ...agent, status: "closed" } },
      };
  }
}

/** Merge a `config_updated` option list into the provider's catalog (creates a
 *  catalog if none was fetched yet). */
function applyCatalogOptions(providerId: string, options: ConfigOption[]) {
  const s = useStructuredStore.getState();
  const existing = s.catalogs[providerId];
  s.setCatalog(providerId, {
    models: existing?.models ?? [],
    config_options: options,
    capabilities: existing?.capabilities ?? emptyCapabilities(),
  });
}

function emptyCapabilities(): ProviderCapabilities {
  return {
    sessionResume: false,
    sessionList: false,
    structuredTools: true,
    reasoningStream: false,
    permissions: false,
    configOptions: false,
    slashCommands: false,
    mcpServers: false,
    images: false,
    contextUsage: false,
  };
}

// ── Actions ──────────────────────────────────────────────────────

/** Generate a fresh structured-agent id. */
function newAgentId(): string {
  return `agent-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

/** Resolve the absolute cwd for a project: the store's project root when the
 *  project is folder/clone-rooted, else `~/CaPilot/workspaces/<name>`. */
async function projectCwd(project: string): Promise<string> {
  const s = useStore.getState();
  const rooted = s.projectRoots[project];
  if (rooted) return rooted;
  const ws = await invoke<string>("workspace_root");
  return `${ws}/${project}`;
}

/** Fetch a provider's catalog (models + config options), caching it in the
 *  store. Costs no model tokens (initialize → session/new → catalog → close). */
export async function fetchProviderCatalog(
  providerId: string,
  cwd: string
): Promise<ProviderCatalog> {
  const st = useStructuredStore.getState();
  const cached = st.catalogs[providerId];
  if (cached) return cached;
  const catalog = await invoke<ProviderCatalog>("agent_provider_catalog", {
    providerId,
    cwd,
  });
  st.setCatalog(providerId, catalog);
  return catalog;
}

/**
 * Create a structured agent under a project and open its AgentPanel tab. The
 * provider defaults to the first available one (e.g. `opencode`); the returned
 * snapshot already carries `session_ready` state, so live `agent://agent-event`
 * flow is applied on top of it. `backend_kind` comes from the daemon's
 * provider list — never hardcoded (handoff §6).
 */
export async function createStructuredAgent(opts: {
  project: string;
  providerId?: string;
  model?: string | null;
  title?: string;
}): Promise<string> {
  const providers = await invoke<ProviderInfo[]>("agent_provider_list");
  const provider = providers.find((p) => p.provider_id === opts.providerId) ?? providers[0];
  if (!provider) throw new Error("没有可用的 Agent 提供方");
  const providerId = provider.provider_id;
  const cwd = await projectCwd(opts.project);
  const agentId = newAgentId();
  const snapshot = await invoke<AgentSnapshot>("agent_create", {
    agentId,
    providerId,
    backendKind: provider.backend_kind,
    cwd,
    model: opts.model ?? null,
    config: [],
  });
  useStructuredStore.getState().setSnapshot(agentId, snapshot);
  // Warm the catalog so the panel's model/config selector renders immediately.
  fetchProviderCatalog(providerId, cwd).catch(() => {});
  useStore.getState().addTab({
    id: agentId,
    type: "structured",
    agentId,
    title: opts.title ?? providerId,
  });
  return agentId;
}

/**
 * Phase 5 default new-agent entry: create a structured agent with the first
 * registered provider (new sessions default to the structured backend,
 * architecture §754). Falls back to the legacy PTY spawn only when no
 * structured provider is registered — keeping the app usable while a provider
 * installs, and preserving the bash/claude template paths.
 */
export async function createDefaultAgent(project?: string): Promise<string> {
  const providers = await invoke<ProviderInfo[]>("agent_provider_list");
  const proj = project ?? "default";
  if (providers.length > 0) {
    return createStructuredAgent({ project: proj });
  }
  const { spawnAgent } = await import("./agentActions");
  return spawnAgent(proj);
}

/** Reconnect replay: fetch the latest snapshot for an agent not in the store
 *  (e.g. created by a previous GUI launch, still alive in the daemon). Events
 *  that raced this fetch were buffered by `applyEvent` and are replayed by
 *  `setSnapshot`. */
export async function refreshStructuredAgent(agentId: string): Promise<void> {
  const st = useStructuredStore.getState();
  if (st.agents.has(agentId)) return;
  const snapshot = await invoke<AgentSnapshot>("agent_snapshot", { agentId });
  st.setSnapshot(agentId, snapshot);
}

/** Start a turn with the unified Composer (architecture §14). */
export async function startStructuredTurn(
  agentId: string,
  text: string
): Promise<void> {
  const trimmed = text.trim();
  if (!trimmed) return;
  await invoke<string>("agent_start_turn", {
    agentId,
    clientMessageId: `cm-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
    text: trimmed,
  });
}

/** Interrupt the agent's in-flight turn. */
export async function interruptStructuredTurn(agentId: string): Promise<void> {
  await invoke("agent_interrupt_turn", { agentId });
}

/** Resolve a permission request with one of its provider-native actions. */
export async function respondStructuredPermission(
  agentId: string,
  requestId: string,
  actionId: string
): Promise<void> {
  await invoke("agent_respond_permission", { agentId, requestId, actionId });
}

/** Set a config option (model, mode, thinking, sandbox, …). */
export async function setStructuredConfig(
  agentId: string,
  configId: string,
  value: ConfigValue
): Promise<void> {
  await invoke("agent_set_config", { agentId, configId, value });
}

/** Close a structured agent: release its live ACP session (the record stays
 *  resumable), close the tab, drop the view. */
export async function closeStructuredAgent(agentId: string): Promise<void> {
  useStore.getState().closeTab(agentId);
  useStructuredStore.getState().removeAgent(agentId);
  try {
    await invoke("agent_close_structured", { agentId });
  } catch {
    // Backend already gone — the tab is closed regardless.
  }
}

// ── Event subscription (app-lifetime) ────────────────────────────

/**
 * Subscribe to the daemon's `agent://agent-event` stream. Every structured event
 * carries `{ id, seq, event }`; `applyEvent` drops anything ≤ the applied
 * watermark and buffers events for agents whose snapshot hasn't loaded yet.
 * Mounted once in App.tsx.
 */
export function useStructuredAgentEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ id: string; seq: number; event: AgentEvent }>(
      "agent://agent-event",
      (e) => {
        if (cancelled) return;
        useStructuredStore
          .getState()
          .applyEvent(e.payload.id, e.payload.seq, e.payload.event);
        // Task auto-complete (Phase 5): a finished structured turn moves the
        // session's in-flight (assigned) todo tags to 待验收. The hook-driven
        // working → idle trigger was retired with the legacy Agent path.
        if (e.payload.event.event === "turn_completed") {
          useStore.getState().completeAssignedTodos(e.payload.id);
        }
      }
    )
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      })
      .catch(() => {
        // Backend not ready — the daemon will re-push when it is.
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
