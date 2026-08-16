import { create } from "zustand";
import { invoke, Channel } from "@tauri-apps/api/core";
import { THEMES, DEFAULT_THEME_ID } from "./themes";
import { playConfirmationSound } from "./sound";
import {
  emptyAcpSession,
  type AcpEventPayload,
  type AcpItem,
  type AcpSessionState,
} from "./acpTypes";
import { isAcpRuntime } from "./runtimeTransport";

// ── Types ───────────────────────────────────────────────────────

export type AgentStatus =
  | "idle"
  | "running"
  | "waiting_input"
  | "awaiting_choice"
  | "busy"
  | "done"
  | "failed";
export type PermissionMode = string;
export type Speed = string;
/** UI font size preset. Base `"s"` is the smallest; larger presets scale the
 *  CSS `--fs-*` tokens. */
export type FontScale = "s" | "m" | "l" | "xl" | "xxl";
/** Visual theme preset. Every theme keeps the IDE's pixel/terminal structure
 *  while swapping its material, phosphor and syntax-color system. */
export type ThemeId = string;

/**
 * Provider's estimate of the CURRENT active-context occupancy for an agent
 * (wire camelCase; backend `AgentUsage`, serialized via serde rename_all).
 *
 * `contextWindowUsedTokens` / `contextWindowMaxTokens` are NOT cumulative
 * token-spend counters: compaction can reduce `contextWindowUsedTokens`, and
 * `contextWindowMaxTokens` is the selected model's capacity (never guessed from
 * visible text). Both stay optional — a provider with no trustworthy value
 * omits instead of estimating.
 *
 * `cacheHitTokens` / `cacheTotalInputTokens` are SESSION-CUMULATIVE prompt
 * token counts feeding the cache hit rate (`cacheHitTokens /
 * cacheTotalInputTokens`). Runtimes normalize their own accounting into this
 * pair (e.g. codex's `input_tokens` already includes cached reads, Claude's
 * does not).
 *
 * `actualModel` is the last model id observed in provider-owned session data.
 * It is display-only and never replaces `AgentInfo.model`, which remains the
 * configured model used by switching and catalog matching.
 */
export interface AgentUsage {
  contextWindowUsedTokens: number | null;
  contextWindowMaxTokens: number | null;
  cacheHitTokens: number | null;
  cacheTotalInputTokens: number | null;
  actualModel: string | null;
}

export interface AgentInfo {
  id: string;
  workspace_id?: string | null;
  /** Stable owning project supplied from persistence; cwd is execution-only. */
  project?: string;
  runtime: string;
  status: AgentStatus;
  title: string;
  cwd: string;
  pid: number | null;
  /** Provider-specific permission mode id. */
  mode?: string;
  /** Provider-specific thinking/effort option id. */
  speed?: string;
  /** Selected model id, or null/undefined for the runtime default. */
  model?: string | null;
  /** Session creation epoch-ms — the sidebar `tm-time` count-up is anchored to
   *  this real timestamp (NOT the activity heartbeat). Restored sessions carry
   *  the DB `created_at`; fresh spawns get `Date.now()` at spawn. */
  createdAt?: number;
  /** Live context-window occupancy pushed by the agent adapter (see
   *  `AgentUsage`). Daemon-memory only; absent until the first sample. */
  last_usage?: AgentUsage | null;
}

/** Hook-reported lifecycle status read from the backend sidecar
 *  (`~/CaPilot/status/<agent_id>.json`, written by the claude adapter's
 *  `--settings` hooks). `status`: `idle` | `working` | `waiting_input` |
 *  `awaiting_choice` | `dormant`; `ts` is epoch seconds. `waiting_input` = a
 *  permission/approval prompt; `awaiting_choice` = a question prompt (claude
 *  AskUserQuestion / opencode `question.asked`) — the tab bar renders them as
 *  待确认 and 待选择 respectively. Absent for non-claude runtimes. */
export interface HookStatus {
  status: string;
  ts: number;
}

/** A registered git worktree isolation project (Rust `WorktreeMeta`, serialized
 *  snake_case — no serde rename_all). `path` is the checked-out worktree dir;
 *  `branch` its dedicated branch. `repo` is the source repository the worktree
 *  was created from. The project shell (`~/CaPilot/workspaces/<name>/`) mirrors
 *  `path` as its root. */
export interface WorktreeMeta {
  id: string;
  repo: string;
  path: string;
  branch: string;
  base_ref: string | null;
  parent_id: string | null;
  instance_id: string;
  created_at: number;
  updated_at: number;
}

/**
 * Live-status derivation (Orca-aligned). The persisted `status` field is a
 * lifecycle record — "running" means the process was alive when the row was
 * last written, and it is NOT corrected on app quit or PTY channel loss.
 * Treating it as live state makes a restored-but-dead session display as
 * "运行中" in the tab bar. Derive the true "connected" signal from the PTY
 * channel instead: attached on spawn/resume, absent for restored or dead
 * sessions. Terminal states (`done`/`failed`/`waiting_input`/`busy`) are
 * authoritative regardless of connectivity.
 *
 * `hook` is the claude hook-reported lifecycle status (authoritative where
 * present). `waiting_input` defers to recency: a permission prompt is a static
 * screen, but the tool that runs after the user grants it streams output — that
 * must read as 运行中, not 待确认.
 */
export function effectiveAgentStatus(
  agent: AgentInfo | undefined,
  connected: boolean,
  active: boolean,
  hook?: HookStatus | null,
  submittedAt?: number
): AgentStatus | "dormant" {
  if (!agent) return "idle";
  if (agent.status === "done") return "done";
  if (agent.status === "failed") return "failed";
  // No live PTY / ACP host → dormant (restored after restart, sleepProject, killed-kept).
  // These sessions are resumable, not dead and not running.
  // ACP "connected" is `acpSessions.live` (see TabBar); never require a PTY channel.
  if (!connected) return "dormant";
  // ACP has no hook sidecar — derive from AgentInfo.status + submit flash.
  if (isAcpRuntime(agent.runtime)) {
    if (agent.status === "waiting_input" || agent.status === "awaiting_choice") {
      return agent.status;
    }
    if (agent.status === "busy" || agent.status === "running") return "running";
    if (submittedAt && Date.now() - submittedAt < SUBMIT_FLASH_MS) return "running";
    return agent.status === "idle" ? "idle" : agent.status;
  }
  if (hook) {
    switch (hook.status) {
      case "working":
        return "running";
      case "idle":
        // A just-submitted prompt lags the hook by up to a poll tick
        // (UserPromptSubmit hasn't landed yet) while the TUI echo/output is
        // already flowing — read that brief window as 运行中. Terminal typing
        // never sets `submittedAt`, so it stays 空闲.
        return submittedAt && Date.now() - submittedAt < SUBMIT_FLASH_MS
          ? "running"
          : "idle";
      case "waiting_input":
        return active ? "running" : "waiting_input";
      case "awaiting_choice":
        // Same recency override as `waiting_input`: the question screen is a
        // static TUI frame, but the output the agent streams after the user
        // answers must read as 运行中, not a lingering 待选择.
        return active ? "running" : "awaiting_choice";
      case "dormant":
        return "dormant";
      default:
        break;
    }
  }
  if (agent.status === "waiting_input") return "waiting_input";
  // `busy` is the explicit "working" flag; otherwise a connected session is
  // 运行中 while it has produced/consumed activity recently, else 空闲.
  if (agent.status === "busy") return "running";
  return active ? "running" : "idle";
}

/**
 * A connected session counts as 运行中 while activity (PTY output or user
 * input) arrived within this window. The old false 运行中 reports had three
 * sources, all now fixed: boot output (wake markers), TUI mouse-tracking
 * reports read as activity (DECSET filter + no activity stamp on click), and
 * idle redraws (verified static for claude/opencode). With those gone the
 * window is a sound recency heuristic again for the non-hook runtimes
 * (codex/opencode/bash). claude sessions are overridden by hook-reported
 * status (see effectiveAgentStatus), so this window never makes claude show a
 * false 运行中 either.
 */
export const ACTIVE_WINDOW_MS = 2000;

/** How long a just-submitted prompt is allowed to read as 运行中 while its
 *  lifecycle hook hasn't caught up yet (UserPromptSubmit lags the send by up to
 *  a poll tick). Only `markAgentSubmitted` (the Composer send path) sets the
 *  marker, so typing in the terminal never triggers this. */
export const SUBMIT_FLASH_MS = 1500;

/** Minimum gap between activity stamps (appendAgentOutput / markAgentActive).
 *  Kept small (0.5s) so a future non-zero window still tracks liveness without
 *  re-rendering the tab strip on every PTY chunk. */
export const ACTIVE_STAMP_THROTTLE_MS = 500;

export interface RuntimeInfo {
  id: string;
  name: string;
  available: boolean;
  authenticated: boolean;
  /** CLI version string reported by `<binary> --version`, when detectable. */
  version?: string | null;
  models?: {
    id: string;
    name: string;
    provider: string;
    is_default: boolean;
    /** Per-model reasoning efforts (codex). Absent = not exposed per model. */
    efforts?: {
      id: string;
      label: string;
      description: string;
      is_default: boolean;
    }[];
  }[];
  /** Provider-native permission choices. Empty means this runtime has no
   * permission policy control (for example Bash). */
  permission_modes?: PermissionModeInfo[];
  thinking_options?: ThinkingOptionInfo[];
  /** `"pty"` (default) or `"acp"`. Prefer this / `id.startsWith("acp:")` over name hard-codes. */
  transport?: "pty" | "acp" | string;
}

export interface ThinkingOptionInfo {
  id: Speed;
  label: string;
  description: string;
}

export interface PermissionModeInfo {
  id: PermissionMode;
  label: string;
  description: string;
  requires_confirmation: boolean;
}

// ── Rate-limit usage (Settings → 已安装 → ⚙ → 用量统计, status bar) ──

export interface UsageWindow {
  /** "5h" | "7d" | "30d" (provider window length). */
  label: string;
  window_minutes: number;
  /** Percent of the window already used (0..100). */
  used_pct: number | null;
  /** 100 - used_pct — the "剩余用量" the status bar shows. */
  remaining_pct: number | null;
  /** Epoch seconds the window resets. */
  resets_at: number | null;
}

export interface RuntimeUsage {
  runtime: string;
  available: boolean;
  /** Human reason when unavailable (shown in the settings availability check). */
  error: string | null;
  /** e.g. codex "plus". */
  plan_type: string | null;
  windows: UsageWindow[];
  checked_at: number;
}

/** Per-runtime fetch config persisted under the `usage_config` setting. */
export interface UsageConfig {
  /** opencode.ai `auth` cookie value (bare token, or a full `k=v` pair). */
  auth_cookie?: string;
  /** Workspace id from `https://opencode.ai/workspace/<id>/go`; empty → probe. */
  workspace_id?: string;
}

// ── App self-update ─────────────────────────────────────────────

export type UpdateStatusKind =
  | "idle"
  | "checking"
  | "available"
  | "up-to-date"
  | "error";

/** Wire shape of the Rust `update_check` command (serde camelCase). */
export interface UpdateStatus {
  currentVersion: string;
  latestVersion: string | null;
  available: boolean;
  notes: string | null;
  publishedAt: string | null;
  target: string;
  installable: boolean;
}

/** One job inside a CI workflow run (developer tool). */
export interface CiJob {
  id: number;
  name: string;
  status: string;
  conclusion: string | null;
}

/** One workflow run (a Release pipeline execution for the current tag). */
export interface CiRun {
  id: number;
  name: string;
  status: string;
  conclusion: string | null;
  /** 0..1 overall progress across the run's jobs. */
  progress: number;
  jobs: CiJob[];
  title: string;
}

/** Wire shape of the Rust `ci_status` command. */
export interface CiStatus {
  tag: string;
  run: CiRun | null;
  error: string | null;
}

export interface Tab {
  id: string;
  type: "agent" | "editor" | "diff" | "image";
  agentId?: string;
  /** Editor / image: absolute file path. Diff: the "new" (worktree/index) side
   *  path, used for project grouping in the tab bar. */
  filePath?: string;
  /** Diff tabs carry a snapshot of the two sides at open time. */
  diffOld?: string;
  diffNew?: string;
  title: string;
}

// ── File content search (right sidebar Files tab) ─────────────────

/** One match inside a file, from the `fs_search` backend command. `column` /
 *  `matchLength` are char offsets into the *raw* line; `displayColumn` /
 *  `displayMatchLength` are char offsets into the possibly-truncated
 *  `lineContent` (they differ when the backend clamped a long line).
 *  The frontend highlights `lineContent` with the display_* pair. */
export interface ContentSearchMatch {
  line: number;
  column: number;
  matchLength: number;
  lineContent: string;
  displayColumn?: number;
  displayMatchLength?: number;
}

export interface ContentSearchFileResult {
  filePath: string;
  relativePath: string;
  matches: ContentSearchMatch[];
  matchCount?: number;
}

export interface ContentSearchResult {
  files: ContentSearchFileResult[];
  totalMatches: number;
  truncated: boolean;
}

/** Per-project-root content-search state (query, options, results, collapsed
 *  files). Kept in the store so switching right-sidebar tabs doesn't lose the
 *  query. `searchId` is bumped per execution; async results only land if they
 *  carry the latest id (anti-race). */
export interface FileSearchState {
  query: string;
  caseSensitive: boolean;
  wholeWord: boolean;
  useRegex: boolean;
  includePattern: string;
  excludePattern: string;
  results: ContentSearchResult | null;
  loading: boolean;
  /** absolute filePath → collapsed in the result list. */
  collapsed: Record<string, boolean>;
  searchId: number;
}

export const defaultFileSearchState = (): FileSearchState => ({
  query: "",
  caseSensitive: false,
  wholeWord: false,
  useRegex: false,
  includePattern: "",
  excludePattern: "",
  results: null,
  loading: false,
  collapsed: {},
  searchId: 0,
});

// ── Todo tags (overview task tracker) ─────────────────────────────

/** Drag-payload MIME for todo tags. Distinct from the `text/plain` payloads
 *  already in use (tab ids / project names / file paths) so every drop target
 *  can tell a tag drag from a tab drag by checking `types.includes(...)`. */
export const TODO_DRAG_MIME = "application/x-capilot-todo";

/**
 * One task tag shown in the right sidebar's 概览 tab. Lifecycle:
 * `todo` (待分配, visible) → `assigned` (dropped onto a session, in-flight and
 * invisible) → `done` (待验收, visible with the session name). `done` is reached
 * automatically when the assigned session's hook goes working → idle, or
 * manually via the check button on an unassigned tag.
 */
export interface TodoTag {
  id: string;
  text: string;
  /** todo=待分配(可见) · assigned=已分配 in-flight(不可见) · done=待验收(可见) */
  status: "todo" | "assigned" | "done";
  /** The session the tag was assigned to (assigned/done). */
  agentId?: string | null;
  /** Session name shown on the 待验收 tag. */
  sessionName?: string | null;
  /** Owning project; null = global. Assigned tags take the session's project. */
  project?: string | null;
  createdAt: number;
  doneAt?: number | null;
}

// ── Split layout ──────────────────────────────────────────────────
//
// The content area is a recursive binary tree of panes. A leaf holds a tab id;
// a split node divides its box between two children along a row (side-by-side)
// or column (stacked) axis, with `ratio` = fraction taken by `a`. This
// generalizes the old two-pane model to arbitrary rows × columns (2×2 grid,
// 3 columns, nested splits, …).

export type SplitNode =
  | { kind: "leaf"; id: string; tabId: string }
  | {
      kind: "split";
      id: string;
      direction: "row" | "column";
      ratio: number;
      a: SplitNode;
      b: SplitNode;
    };

let splitUidSeq = 0;
function splitUid(): string {
  splitUidSeq += 1;
  return `sp-${splitUidSeq.toString(36)}`;
}

/** Tab ids of every leaf, in layout order (left-to-right, top-to-bottom). */
export function splitLeafTabIds(node: SplitNode): string[] {
  return node.kind === "leaf"
    ? [node.tabId]
    : [...splitLeafTabIds(node.a), ...splitLeafTabIds(node.b)];
}

/** Is `tabId` currently on screen? With a split the visible set is every leaf;
 *  without one it's the single active tab. Used to decide whether a completed
 *  agent run's result is unviewed (see `unreadCompletion`). */
function tabIsVisible(
  tabId: string,
  activeTabId: string | null,
  splitTree: SplitNode | null
): boolean {
  if (splitTree) return splitLeafTabIds(splitTree).includes(tabId);
  return activeTabId === tabId;
}

/** The first (leftmost/topmost) leaf's tab id — the pane that `setActiveTab` /
 *  `addTab` bring a hidden tab into while a split is active. */
function splitFirstLeaf(node: SplitNode): string {
  return node.kind === "leaf" ? node.tabId : splitFirstLeaf(node.a);
}

/** Number of leaves in the tree. */
function splitLeafCount(node: SplitNode): number {
  return node.kind === "leaf" ? 1 : splitLeafCount(node.a) + splitLeafCount(node.b);
}

/** Replace the leaf holding `targetTabId` with a leaf holding `newTabId`. */
function splitReplaceLeaf(
  node: SplitNode,
  targetTabId: string,
  newTabId: string
): SplitNode {
  if (node.kind === "leaf") {
    return node.tabId === targetTabId ? { ...node, tabId: newTabId } : node;
  }
  return {
    ...node,
    a: splitReplaceLeaf(node.a, targetTabId, newTabId),
    b: splitReplaceLeaf(node.b, targetTabId, newTabId),
  };
}

/** Split the leaf holding `targetTabId` into two leaves along `direction`;
 *  when `newOnFirst` the new tab takes the `a` (left/top) side. */
function splitInsert(
  node: SplitNode,
  targetTabId: string,
  newTabId: string,
  direction: "row" | "column",
  newOnFirst: boolean
): SplitNode {
  if (node.kind === "leaf") {
    if (node.tabId !== targetTabId) return node;
    const existing: SplitNode = { kind: "leaf", id: splitUid(), tabId: targetTabId };
    const fresh: SplitNode = { kind: "leaf", id: splitUid(), tabId: newTabId };
    return {
      kind: "split",
      id: splitUid(),
      direction,
      ratio: 0.5,
      a: newOnFirst ? fresh : existing,
      b: newOnFirst ? existing : fresh,
    };
  }
  return {
    ...node,
    a: splitInsert(node.a, targetTabId, newTabId, direction, newOnFirst),
    b: splitInsert(node.b, targetTabId, newTabId, direction, newOnFirst),
  };
}

/** Remove the leaf holding `tabId`, collapsing its parent split to the sibling.
 *  Returns null when the whole tree is gone. */
function splitRemoveLeaf(node: SplitNode | null, tabId: string): SplitNode | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.tabId === tabId ? null : node;
  const a = splitRemoveLeaf(node.a, tabId);
  const b = splitRemoveLeaf(node.b, tabId);
  if (!a) return b;
  if (!b) return a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Set the ratio of the split node with the given id. */
function splitSetRatio(node: SplitNode, nodeId: string, ratio: number): SplitNode {
  if (node.kind === "leaf") return node;
  if (node.id === nodeId) return { ...node, ratio };
  return {
    ...node,
    a: splitSetRatio(node.a, nodeId, ratio),
    b: splitSetRatio(node.b, nodeId, ratio),
  };
}

/** Matches the Rust `AgentSessionRecord` (snake_case keys). */
export interface RestoredSession {
  id: string;
  workspace_id: string | null;
  project: string;
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
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Resolve an agent to the same project key used by the sidebar. Persisted
 * project identity wins; cwd parsing exists only for legacy records. */
function projectOfAgent(agent: AgentInfo): string {
  return agent.project || projectOfCwd(agent.cwd);
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

/** Cap ACP transcript rows per session (MVP in-memory only). */
const MAX_ACP_ITEMS = 2000;

interface AppState {
  // Agents
  agents: Map<string, AgentInfo>;
  agentChannels: Map<string, Channel<number[]>>;
  /**
   * In-memory ACP session view-models (messages / tools / turn flags).
   * Keyed by agent id. Presence of a live entry also acts as the ACP
   * "connected" signal (parallel to `agentChannels` for PTY).
   */
  acpSessions: Map<string, AcpSessionState>;
  /** Last input/output activity epoch-ms per agent, for the 运行中/空闲 split.
   *  Updated throttled (≈1/s) so a streaming PTY doesn't re-render the tab
   *  strip per chunk. */
  agentActiveAt: Map<string, number>;
  /** "Waking" marker (epoch-ms set, value unused) per agent. Set whenever a
   *  process is freshly spawned or a dormant session is resumed — the CLI is
   *  booting / showing its prompt, not working. While set, output is excluded
   *  from the activity stamp; the first user engagement (see markAgentActive)
   *  clears it so real task output reads as 运行中. */
  agentWakeAt: Map<string, number>;
  /** Hook-reported lifecycle status per agent (claude only; see `HookStatus`).
   *  Polled from the backend sidecar; `effectiveAgentStatus` prefers it over the
   *  activity heuristic so the 运行中/空闲 split follows real claude lifecycle
   *  events (submit → working, stop → idle, permission → waiting). */
  hookStatus: Map<string, HookStatus>;
  /** Epoch-ms of the last Composer prompt submission per agent. Bridges the
   *  window where a just-sent prompt's lifecycle hook hasn't caught up yet
   *  (UserPromptSubmit lags the send) — see `SUBMIT_FLASH_MS`. Never set by
   *  terminal typing. */
  agentSubmittedAt: Map<string, number>;
  /** Agent ids whose last completed turn's result hasn't been viewed yet (the
   *  tab bar reads 已完成 instead of 空闲). Set when the hook goes working →
   *  idle while the agent's tab is off-screen; cleared on view (`setActiveTab`)
   *  or when the user submits a new prompt. In-memory only. */
  unreadCompletion: Set<string>;
  /** Per-agent tab-label flash request (agentId → sequence). Each running →
   *  other transition increments the seq so the tab bar can restart the flash
   *  animation even when two transitions land in the same frame. */
  tabFlash: Map<string, number>;
  /** Output buffered before a terminal attached (and between mounts). */
  agentOutputs: Map<string, number[]>;
  /** Whether the initial persisted-session lookup has settled. */
  sessionsRestored: boolean;
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
  /** Latest fetched remaining-usage per runtime (codex/opencode). */
  usageState: Record<string, RuntimeUsage>;
  /** Bumped when settings change usage config/enabled — re-triggers the poller. */
  usageRevision: number;

  // UI tabs
  tabs: Tab[];
  activeTabId: string | null;

  // Composer
  composerOpen: boolean;
  /** One-shot focus directive for the F1 input↔terminal toggle. `seq` bumps on
   *  every request so subscribers can skip stale/mount-time values. */
  focusRequest: { target: "composer" | "terminal"; seq: number } | null;
  /** One-shot Ctrl+F search directive routed to the active panel (terminal or
   *  editor). Same `seq` discipline as `focusRequest`. */
  searchRequest: { target: "terminal" | "editor"; seq: number } | null;
  /** One-shot "open file at line/column" directive from a content-search result
   *  click. Consumed by the matching EditorPanel after its document loads.
   *  Same `seq` discipline as `focusRequest`/`searchRequest`. */
  revealRequest: { filePath: string; line: number; column?: number; seq: number } | null;
  /** Content-search state per project root (see `FileSearchState`). */
  fileSearchByRoot: Record<string, FileSearchState>;
  permissionMode: PermissionMode;
  speed: Speed;
  /** Runtime model id chosen via composer `[模型↑]` (null = runtime default). */
  selectedModel: string | null;
  draftHistory: string[];
  draftIndex: number;

  // Split layout — a recursive binary tree of panes (a leaf holds a tab id; a
  // split node divides its box along a row/column axis). Null = the default
  // single-panel view.
  splitTree: SplitNode | null;
  /** Tab id currently being dragged (for edge-drop feedback). */
  draggedTabId: string | null;

  // Projects (workspace dirs under ~/CaPilot/workspaces/<name>)
  projects: string[];
  /** project name → absolute root path (from list_projects / create_project). */
  projectRoots: Record<string, string>;
  /** Single-select focused project; null = unfocused (tab bar shows all tabs). */
  focusedProject: string | null;
  /** In-flight git clones keyed by clone id (id → project name). A project
   *  listed here renders "正在克隆中" in the sidebar and is dropped on failure. */
  pendingClones: Record<string, string>;
  /** Registered git worktree isolation projects (see `WorktreeMeta`). Drives
   *  the sidebar branch badge + the dedicated 移除工作区 path. */
  worktrees: WorktreeMeta[];

  // Sidebars
  leftSidebarOpen: boolean;
  rightSidebarOpen: boolean;
  leftWidth: number;
  rightWidth: number;

  // Todo tags (overview task tracker)
  todos: TodoTag[];
  /** 概览 scope: `"global"` = tags with `project == null`; `"project"` = tags
   *  of the focused project. Toggled by the overview tab button itself. */
  todoScope: "global" | "project";

  // Composer height
  /** Composer height (px). */
  composerH: number | null;

  // Resource monitor (DevPlan §10)
  agentResources: Map<string, ResourcePoint>;

  // Onboarding
  onboarded: boolean;

  // New-project modal (opened from sidebar "+" / empty row / main-area empty state)
  nprojOpen: boolean;

  // UI font size preset ("s" | "m" | "l" | "xl" | "xxl"); base = smallest.
  fontScale: FontScale;

  // Visual theme preset, reflected to <html data-theme="…"> by App.
  themeId: ThemeId;

  /** Whether the running → other transition chime is enabled (default true). */
  soundEnabled: boolean;

  /** Runtime spawned by Ctrl+T (preference). The effective pick is resolved by
   *  `resolveCtrlTRuntime` (configured → claude → bash → hint). */
  ctrlTRuntime: string;

  // App self-update. State is written by
  // `ui/state/update.ts`; only the auto-check toggle has a dedicated action.
  /** Real app version, read at runtime via getVersion(). */
  currentVersion: string | null;
  updateStatus: UpdateStatusKind;
  /** Latest published version string when an update is available. */
  updateLatest: string | null;
  updateNotes: string | null;
  updateError: string | null;
  updateDownloading: boolean;
  /** 0..1 download progress while installing. */
  updateProgress: number | null;
  /** Whether the running build supports in-app install (false = dev build). */
  updateInstallable: boolean;
  /** Latest version already surfaced via notification this session (dedup —
   *  a fresh update is announced once per app launch, not every check). */
  updateNotifiedVersion: string | null;
  /** Whether the app auto-checks for updates on startup (default true). */
  autoCheckUpdate: boolean;

  // Developer tool: CI build status for the current version (GitHub Actions).
  /** Latest polled CI status; `null` before the first poll. */
  ciStatus: CiStatus | null;
  /** True while a `ci_status` request is in flight. */
  ciPolling: boolean;

  // Actions
  addAgent: (info: AgentInfo, channel: Channel<number[]> | null, createdAtTs?: number) => void;
  /**
   * Register a freshly spawned/resumed ACP session as live (no PTY channel).
   * Creates an empty transcript buffer if missing.
   */
  markAcpLive: (id: string) => void;
  /** Apply one `acp://event` envelope (already scoped by agentId). */
  applyAcpEvent: (payload: AcpEventPayload) => void;
  removeAgent: (id: string) => void;
  updateAgentStatus: (id: string, status: AgentStatus) => void;
  /** Replace an agent's live context-window usage (`null` = runtime has no
   *  trustworthy value). Separate from the rate-limit `setUsage` — do not
   *  merge the two. */
  updateAgentUsage: (id: string, usage: AgentUsage | null) => void;
  /** Rename a terminal: updates the live agent record AND the tab's title
   *  snapshot, so the tab bar + sidebar labels move together immediately. The
   *  backend (`agent_rename`) already persisted the new title. */
  updateAgentTitle: (id: string, title: string) => void;
  appendAgentOutput: (id: string, data: number[]) => void;
  clearAgentOutput: (id: string) => void;
  /** Stamp an agent as active (user input sent to its PTY). Combined with
   *  output activity in `appendAgentOutput`, drives the 运行中/空闲 split. */
  markAgentActive: (id: string) => void;
  /** Stamp an agent as having just received a submitted prompt. Sets the
   *  `agentSubmittedAt` flash marker so the tab reads 运行中 while the
   *  lifecycle hook catches up; clears the activity-based wakeup marker. */
  markAgentSubmitted: (id: string) => void;
  /** Update (or clear with `null`) an agent's hook-reported lifecycle status. */
  setHookStatus: (id: string, hook: HookStatus | null) => void;
  /** Request the tab bar to flash an agent's tab label twice (a running → other
   *  transition just happened). */
  flashTab: (id: string) => void;
  /** Mark/unmark an agent's unviewed-completion flag (已完成). `unread` true
   *  flags a finished turn the user hasn't opened; false clears it. */
  setAgentUnread: (id: string, unread: boolean) => void;
  requestResume: (id: string) => void;
  consumeResume: (id: string) => void;
  /** Drop a finished agent's dead channel, keeping its record + output so the
   *  sidebar "已结束" group can reopen (resume) it. */
  dropAgentChannel: (id: string) => void;
  setSessionsRestored: () => void;
  setRuntimes: (runtimes: RuntimeInfo[]) => void;
  setUsage: (runtime: string, usage: RuntimeUsage) => void;
  bumpUsageRevision: () => void;
  addTab: (tab: Tab) => void;
  /** Add a tab without changing the active tab (used by session restore). */
  addTabSilent: (tab: Tab) => void;
  /** Move a tab to a new index in the tab strip (drag-reorder). */
  reorderTabs: (fromIndex: number, toIndex: number) => void;
  closeTab: (id: string) => void;
  setActiveTab: (id: string) => void;
  setSplitTree: (tree: SplitNode | null) => void;
  /** Split the pane showing `targetTabId` so `newTabId` joins it along
   *  `direction` (`newOnFirst` → the new tab takes the left/top side). */
  splitPane: (
    targetTabId: string,
    newTabId: string,
    direction: "row" | "column",
    newOnFirst: boolean
  ) => void;
  /** Set the ratio of a specific split node (by id). */
  setSplitRatio: (nodeId: string, ratio: number) => void;
  setDraggedTabId: (id: string | null) => void;
  toggleComposer: () => void;
  requestFocus: (target: "composer" | "terminal") => void;
  requestSearch: (target: "terminal" | "editor") => void;
  /** Ask the editor showing `filePath` to scroll its cursor to line/column. */
  requestReveal: (filePath: string, line: number, column?: number) => void;
  /** Merge a patch into a root's content-search state (creates the default on
   *  first touch). */
  updateFileSearch: (root: string, patch: Partial<FileSearchState>) => void;
  /** Drop a root's content-search state (e.g. on root switch). */
  clearFileSearch: (root: string) => void;
  setPermissionMode: (mode: PermissionMode) => void;
  setSpeed: (speed: Speed) => void;
  setSelectedModel: (model: string | null) => void;
  pushDraft: (text: string) => void;
  navigateDraft: (dir: -1 | 1) => string | null;
  toggleLeftSidebar: () => void;
  toggleRightSidebar: () => void;
  setLeftWidth: (width: number) => void;
  setRightWidth: (width: number) => void;
  setComposerH: (height: number | null) => void;
  setProjects: (projects: string[]) => void;
  setProjectRoots: (roots: Record<string, string>) => void;
  setFocusedProject: (name: string | null) => void;
  projectRoot: (name: string) => string | undefined;
  addProject: (name: string, root?: string) => void;
  removeProject: (name: string) => void;
  /** Track a background `git_clone`; `name` shows as "正在克隆中" until
   *  `finishClone` (from `git://cloned` / `git://clone-error`). */
  beginClone: (id: string, name: string) => void;
  finishClone: (id: string) => void;
  /** Move `name` to `targetName`'s position in the sidebar project list. */
  moveProject: (name: string, targetName: string) => void;
  /** Replace the whole worktree registry (from `worktree_list_all` on mount). */
  setWorktrees: (list: WorktreeMeta[]) => void;
  /** Register a created worktree (backend `worktree://created` / create modal).
   *  `name` is the CaPilot project shell name (may carry a `-N` dedupe suffix). */
  addWorktree: (meta: WorktreeMeta, name: string) => void;
  /** Drop a worktree locally WITHOUT backend calls (backend `worktree://removed`
   *  already cleaned the shell/DB). Also used by `removeWorktree` after the
   *  dedicated remove succeeds. */
  removeWorktreeLocal: (path: string) => void;
  /** Dedicated 移除工作区: locally drop the project + worktree, then ask the
   *  backend to kill sessions, delete the shell and `git worktree remove`. */
  removeWorktree: (path: string) => void;
  /** Sleep a project: kill all its agent processes + close its tabs/panels to
   *  free CPU/memory. Sessions stay in the DB, so reopening a terminal resumes. */
  sleepProject: (name: string) => void;
  renameProject: (oldName: string, newName: string) => Promise<string>;
  termTemplates: TermTemplate[];
  addTermTemplate: (t: TermTemplate) => void;
  updateTermTemplate: (id: string, patch: Partial<Pick<TermTemplate, "name" | "command">>) => void;
  removeTermTemplate: (id: string) => void;
  applyResourceSample: (resources: AgentResource[]) => void;
  setOnboarded: (onboarded: boolean) => void;
  setNprojOpen: (open: boolean) => void;
  setFontScale: (scale: FontScale) => void;
  setThemeId: (theme: ThemeId) => void;
  /** Persist the Ctrl+T runtime preference (localStorage). */
  setCtrlTRuntime: (runtime: string) => void;
  setTodos: (todos: TodoTag[]) => void;
  addTodo: (text: string) => void;
  updateTodoText: (id: string, text: string) => void;
  assignTodoToAgent: (
    id: string,
    agentId: string,
    sessionName: string | null
  ) => void;
  deleteTodo: (id: string) => void;
  toggleTodoScope: () => void;
  /** Persist the startup auto-check toggle (Settings → 关于). */
  setAutoCheckUpdate: (enabled: boolean) => void;
  /** Persist the completion-chime toggle (Settings → 外观与显示). */
  setSoundEnabled: (enabled: boolean) => void;
  /** Poll the current version's CI build status (developer tool). */
  pollCiStatus: () => void;
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

/** Persisted UI font size preset. Fallback: medium ("m"). */
const FONT_SCALE_KEY = "capilot.fontScale";
const FONT_SCALES: FontScale[] = ["s", "m", "l", "xl", "xxl"];
function loadFontScale(): FontScale {
  try {
    const v = localStorage.getItem(FONT_SCALE_KEY);
    if (v && (FONT_SCALES as string[]).includes(v)) return v as FontScale;
  } catch {
    // storage unavailable — use base
  }
  return "m";
}

/** Persisted visual theme. The quantum lattice look is the safe migration
 *  default for installs created before theme switching existed. */
const THEME_KEY = "capilot.theme";
function loadThemeId(): ThemeId {
  try {
    const value = localStorage.getItem(THEME_KEY);
    if (value && THEMES.some((t) => t.id === value)) return value;
  } catch {
    // storage unavailable — use the default
  }
  return DEFAULT_THEME_ID;
}

/** Persisted Ctrl+T runtime preference (default claude). The effective pick is
 *  resolved against the installed runtimes by `resolveCtrlTRuntime` (configured
 *  → claude → bash → hint). */
const CTRLT_RUNTIME_KEY = "capilot.ctrlTRuntime";
const DEFAULT_CTRLT_RUNTIME = "claude";
function loadCtrlTRuntime(): string {
  try {
    const value = localStorage.getItem(CTRLT_RUNTIME_KEY);
    if (value) return value;
  } catch {
    // storage unavailable — use the default
  }
  return DEFAULT_CTRLT_RUNTIME;
}

/** Effective runtime for Ctrl+T: the configured pick when it's installed,
 *  otherwise claude, otherwise bash (`bash-rc`), otherwise null — the caller
 *  shows the "no runtime" hint instead of spawning. */
export function resolveCtrlTRuntime(
  configured: string,
  runtimes: RuntimeInfo[]
): string | null {
  const available = new Set(
    runtimes.filter((r) => r.available).map((r) => r.id)
  );
  if (available.has(configured)) return configured;
  if (available.has("claude")) return "claude";
  if (available.has("bash-rc")) return "bash-rc";
  return null;
}

// ── New-terminal templates ──────────────────────────────────────
// The project "+" button opens a picker: bash (fixed, always first) / Claude /
// Codex / dsh / user-defined quick-start commands. Custom templates persist
// locally. (opencode was removed as a selectable runtime — see
// `known_runtimes`; persisted templates are dropped on load.)

/** A new-terminal template shown in the project "+" picker. `command` is run
 *  after the shell starts (bash / bash-rc) / ignored for agent runtimes;
 *  `fixed` (bash) can't be renamed or removed.
 *  `runtime` is a runtime id (`claude`, `bash-rc`, `acp:opencode`, …). */
export interface TermTemplate {
  id: string;
  name: string;
  command: string;
  runtime: string;
  fixed?: boolean;
}

const TERM_TEMPLATES_KEY = "capilot.termTemplates";
const DEFAULT_TEMPLATES: TermTemplate[] = [
  { id: "bash-rc", name: "bash", command: "", runtime: "bash-rc", fixed: true },
  { id: "claude", name: "claude", command: "", runtime: "claude" },
  { id: "codex", name: "codex", command: "", runtime: "codex" },
  { id: "dsh", name: "dsh", command: "", runtime: "dsh" },
  { id: "pi", name: "Pi", command: "", runtime: "pi" },
];
function loadTermTemplates(): TermTemplate[] {
  try {
    const raw = localStorage.getItem(TERM_TEMPLATES_KEY);
    const stored: TermTemplate[] = raw ? (JSON.parse(raw) as TermTemplate[]) : [];
    // Drop the old minimal `--norc` "bash" template (superseded by the full
    // bash), re-label the old "正常 bash" default to just "bash", and drop
    // persisted omp / opencode templates (runtimes removed as new terminals).
    const list = stored.filter(
      (t) => t.id !== "bash" && t.id !== "omp" && t.id !== "opencode"
    );
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

// ── Todo persistence ─────────────────────────────────────────────
// The todo list lives in the backend settings KV table (key "todos", a JSON
// array). Every mutating store action writes it back fire-and-forget, matching
// the termTemplates/localStorage pattern. Hydration happens in TodoPanel after
// session restore settles (so orphaned assigned tags can be detected there).

let todoUidSeq = 0;
function todoUid(): string {
  todoUidSeq += 1;
  // Timestamp prefix keeps ids unique across restarts: the seq alone restarts
  // at 0 every launch, which would collide with ids re-hydrated from storage.
  return `todo-${Date.now().toString(36)}-${todoUidSeq.toString(36)}`;
}

function saveTodos(todos: TodoTag[]) {
  invoke("setting_set", { key: "todos", value: JSON.stringify(todos) }).catch(
    () => {}
  );
}

export const useStore = create<AppState>((set, get) => {
  // Local-only removal of a project's UI state (list, root map, focus, tabs,
  // agents) WITHOUT any backend call. `removeProject` and the worktree flows
  // share this; the backend work (delete_project vs worktree_remove) is always
  // issued by the caller.
  const dropProjectLocal = (name: string) => {
    const s = get();
    const projectRoots = { ...s.projectRoots };
    delete projectRoots[name];
    const focusedProject = s.focusedProject === name ? null : s.focusedProject;
    set({
      projects: s.projects.filter((p) => p !== name),
      projectRoots,
      focusedProject,
    });
    // Clear the persisted focus when the focused project itself is removed,
    // so the next startup doesn't try to restore a missing name.
    if (s.focusedProject === name) {
      invoke("setting_set", {
        key: "focused_project",
        value: "",
      }).catch(() => {});
    }
    const doomed: string[] = [];
    s.agents.forEach((a, id) => {
      if (projectOfAgent(a) === name) doomed.push(id);
    });
    for (const id of doomed) {
      invoke("sessions_delete", { id })
        .catch(() => invoke("agent_kill", { id }).catch(() => {}));
      s.closeTab(id);
      s.removeAgent(id);
    }
  };

  return {
    agents: new Map(),
  agentChannels: new Map(),
  acpSessions: new Map(),
  agentActiveAt: new Map(),
  agentWakeAt: new Map(),
  hookStatus: new Map(),
  agentSubmittedAt: new Map(),
  unreadCompletion: new Set(),
  tabFlash: new Map(),
  agentOutputs: new Map(),
  sessionsRestored: false,
  resumeOnOpen: new Set(),
  closedAgentIds: new Set(),
  runtimes: [],
  usageState: {},
  usageRevision: 0,
  tabs: [],
  activeTabId: null,
  composerOpen: true,
  focusRequest: null,
  searchRequest: null,
  revealRequest: null,
  fileSearchByRoot: {},
  permissionMode: "ask",
  speed: "auto",
  selectedModel: null,
  draftHistory: [],
  draftIndex: -1,
  leftSidebarOpen: true,
  rightSidebarOpen: true,
  leftWidth: 248,
  rightWidth: 340,
  composerH: null,
  splitTree: null,
  draggedTabId: null,
  agentResources: new Map(),
  projects: [],
  projectRoots: {},
  focusedProject: null,
  pendingClones: {},
  worktrees: [],
  onboarded: loadOnboarded(),
  nprojOpen: false,
  termTemplates: loadTermTemplates(),
  fontScale: loadFontScale(),
  themeId: loadThemeId(),
  ctrlTRuntime: loadCtrlTRuntime(),
  soundEnabled: true,
  todos: [],
  todoScope: "global",
  currentVersion: null,
  updateStatus: "idle",
  updateLatest: null,
  updateNotes: null,
  updateError: null,
  updateDownloading: false,
  updateProgress: null,
  updateInstallable: false,
  updateNotifiedVersion: null,
  autoCheckUpdate: true,
  ciStatus: null,
  ciPolling: false,

  markAcpLive: (id) =>
    set((s) => {
      if (s.closedAgentIds.has(id)) return {};
      const acpSessions = new Map(s.acpSessions);
      const prev = acpSessions.get(id);
      acpSessions.set(id, prev ? { ...prev, live: true } : emptyAcpSession());
      return { acpSessions };
    }),

  applyAcpEvent: (payload) =>
    set((s) => {
      const id = payload.agentId;
      if (!id || s.closedAgentIds.has(id)) return {};
      const acpSessions = new Map(s.acpSessions);
      const prev = acpSessions.get(id) ?? emptyAcpSession();
      let next: AcpSessionState = { ...prev, items: prev.items.slice() };
      const now = Date.now();
      const push = (item: Omit<AcpItem, "at" | "key"> & { key?: string }) => {
        const row: AcpItem = {
          ...item,
          key: item.key ?? `${id}-${now}-${next.items.length}`,
          at: now,
        };
        next.items.push(row);
        if (next.items.length > MAX_ACP_ITEMS) {
          next.items = next.items.slice(next.items.length - MAX_ACP_ITEMS);
        }
      };

      switch (payload.type) {
        case "session_started":
          next.live = true;
          break;
        case "message_chunk": {
          const role = payload.role ?? "agent";
          const mid = payload.messageId;
          // Append to the last open chunk of the same messageId when possible.
          const last = next.items[next.items.length - 1];
          if (
            last &&
            last.kind === "message" &&
            mid &&
            last.messageId === mid &&
            last.role === role
          ) {
            next.items[next.items.length - 1] = {
              ...last,
              text: last.text + (payload.text ?? ""),
            };
          } else {
            push({
              kind: "message",
              role,
              text: payload.text ?? "",
              messageId: mid,
            });
          }
          break;
        }
        case "tool_call": {
          const existing = next.items.findIndex(
            (it) => it.kind === "tool" && it.toolCallId === payload.toolCallId
          );
          if (existing >= 0) {
            const cur = next.items[existing];
            next.items[existing] = {
              ...cur,
              text: payload.title || cur.text,
              status: payload.status,
            };
          } else {
            push({
              kind: "tool",
              role: "system",
              text: payload.title || payload.toolCallId,
              toolCallId: payload.toolCallId,
              status: payload.status,
            });
          }
          break;
        }
        case "tool_call_update": {
          const existing = next.items.findIndex(
            (it) => it.kind === "tool" && it.toolCallId === payload.toolCallId
          );
          if (existing >= 0) {
            const cur = next.items[existing];
            next.items[existing] = {
              ...cur,
              status: payload.status,
              detail: payload.detail ?? cur.detail,
            };
          } else {
            push({
              kind: "tool",
              role: "system",
              text: payload.toolCallId,
              toolCallId: payload.toolCallId,
              status: payload.status,
              detail: payload.detail,
            });
          }
          break;
        }
        case "plan": {
          const entries = Array.isArray(payload.entries) ? payload.entries : [];
          const text = entries
            .map((e) => {
              if (e && typeof e === "object") {
                const o = e as Record<string, unknown>;
                const content = String(o.content ?? o.title ?? o.text ?? "");
                const st = String(o.status ?? "");
                const mark =
                  st === "completed" ? "☑" : st === "in_progress" ? "►" : "☐";
                return `${mark} ${content}`.trim();
              }
              return String(e);
            })
            .filter(Boolean)
            .join("\n");
          // Replace previous plan row if any.
          const pi = next.items.findIndex((it) => it.kind === "plan");
          if (pi >= 0) {
            next.items[pi] = {
              ...next.items[pi],
              text,
              at: now,
            };
          } else {
            push({ kind: "plan", role: "system", text });
          }
          break;
        }
        case "usage":
          next.usage = {
            used: Number(payload.used) || 0,
            size: Number(payload.size) || 0,
          };
          break;
        case "permission_request":
          next.pendingPermission = {
            requestId: payload.requestId,
            summary: payload.summary,
            toolCallId: payload.toolCallId,
          };
          push({
            kind: "permission",
            role: "system",
            text: payload.summary,
            requestId: payload.requestId,
            toolCallId: payload.toolCallId,
            status: "pending",
            key: `${id}-perm-${payload.requestId}`,
          });
          break;
        case "turn_done":
          next.turnActive = false;
          next.lastStopReason = payload.stopReason;
          push({
            kind: "turn",
            role: "system",
            text: payload.stopReason,
            status: payload.stopReason,
          });
          break;
        case "status": {
          const st = payload.status;
          if (st === "busy" || st === "running") next.turnActive = true;
          if (st === "idle") next.turnActive = false;
          break;
        }
        case "error":
          push({ kind: "error", role: "system", text: payload.message });
          next.turnActive = false;
          break;
        case "stderr":
          push({ kind: "stderr", role: "system", text: payload.line });
          break;
        default:
          break;
      }

      acpSessions.set(id, next);

      // Mirror turn/permission into AgentInfo.status so TabBar/effectiveAgentStatus
      // can show 运行中 / 待确认 without a PTY channel or hook sidecar.
      const agents = new Map(s.agents);
      const agent = agents.get(id);
      let agentsOut = s.agents;
      if (agent) {
        let status = agent.status;
        let patched: AgentInfo = agent;
        if (payload.type === "permission_request") {
          status = "waiting_input";
        } else if (payload.type === "turn_done" || payload.type === "error") {
          status = "idle";
        } else if (
          payload.type === "status" &&
          (payload.status === "busy" || payload.status === "running")
        ) {
          status = "running";
        } else if (payload.type === "status" && payload.status === "idle") {
          status = "idle";
        } else if (
          payload.type === "message_chunk" ||
          payload.type === "tool_call" ||
          payload.type === "tool_call_update"
        ) {
          if (status === "idle" || status === "waiting_input") {
            // streaming after grant — treat as running
            if (status === "idle") status = "running";
          }
        }
        // D9: mirror ACP usage_update onto AgentInfo.last_usage for shared meters.
        if (payload.type === "usage" && next.usage && next.usage.size > 0) {
          patched = {
            ...patched,
            last_usage: {
              contextWindowUsedTokens: next.usage.used,
              contextWindowMaxTokens: next.usage.size,
              cacheHitTokens: null,
              cacheTotalInputTokens: null,
              actualModel: null,
            },
          };
        }
        if (status !== agent.status || patched !== agent) {
          agents.set(id, { ...patched, status });
          agentsOut = agents;
        }
      }

      // Stamp activity for ACP streams (throttled).
      let activeAt = s.agentActiveAt;
      if (
        payload.type === "message_chunk" ||
        payload.type === "tool_call" ||
        payload.type === "tool_call_update" ||
        payload.type === "turn_done"
      ) {
        if (now - (s.agentActiveAt.get(id) ?? 0) >= ACTIVE_STAMP_THROTTLE_MS) {
          activeAt = new Map(s.agentActiveAt);
          activeAt.set(id, now);
        }
      }

      return {
        acpSessions,
        agents: agentsOut,
        agentActiveAt: activeAt,
      };
    }),

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
      // sessions carry the DB `created_at` via `createdAtTs`.
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
        // `info` (spawn/resume payloads) never carries live usage; keep the
        // last sample across addAgent so a resume/model switch doesn't wipe it.
        last_usage:
          info.last_usage !== undefined ? info.last_usage : previous?.last_usage,
      });
      const channels = new Map(s.agentChannels);
      // Only overwrite the PTY channel when a new one is supplied. A restored
      // session has no live PTY yet, so preserve the agent's existing live
      // channel so XTermPanel doesn't lose it and fall into the resume path.
      if (channel) {
        channels.set(info.id, channel);
        // Dormant → connected (resume) or a brand-new spawn: the CLI's welcome
        // screen / idle prompt is not task activity. Mark the session "waking"
        // so appendAgentOutput excludes that output from the 运行中/空闲 signal
        // until the user engages it. A runtime switch (already connected, new
        // channel object) is not a boot and keeps no marker. If the user typed
        // while the resume was in flight (activeAt stamped before this resolve),
        // they already engaged — don't resurrect a wake that would swallow the
        // very task they started.
        const wakeAt = new Map(s.agentWakeAt);
        const now = Date.now();
        const recentlyEngaged =
          now - (s.agentActiveAt.get(info.id) ?? 0) < ACTIVE_WINDOW_MS;
        if ((!previous || !s.agentChannels.has(info.id)) && !recentlyEngaged) {
          wakeAt.set(info.id, now);
        } else {
          wakeAt.delete(info.id);
        }
        return { agents, agentChannels: channels, agentWakeAt: wakeAt };
      }
      return { agents, agentChannels: channels };
    }),

  removeAgent: (id) =>
    set((s) => {
      const agents = new Map(s.agents);
      agents.delete(id);
      const channels = new Map(s.agentChannels);
      channels.delete(id);
      const acpSessions = new Map(s.acpSessions);
      acpSessions.delete(id);
      const activeAt = new Map(s.agentActiveAt);
      activeAt.delete(id);
      const wakeAt = new Map(s.agentWakeAt);
      wakeAt.delete(id);
      const hookStatus = new Map(s.hookStatus);
      hookStatus.delete(id);
      const submittedAt = new Map(s.agentSubmittedAt);
      submittedAt.delete(id);
      const unreadCompletion = new Set(s.unreadCompletion);
      unreadCompletion.delete(id);
      const outputs = new Map(s.agentOutputs);
      outputs.delete(id);
      const resources = new Map(s.agentResources);
      resources.delete(id);
      const resumeOnOpen = new Set(s.resumeOnOpen);
      resumeOnOpen.delete(id);
      const closedAgentIds = new Set(s.closedAgentIds);
      closedAgentIds.add(id);
      // Orphan revert: an in-flight (assigned) tag tied to the deleted session
      // would otherwise stay invisible forever — bring it back to 待分配. Its
      // scope is untouched (a global tag returns to the global view).
      const todos = s.todos.map((t) =>
        t.status === "assigned" && t.agentId === id
          ? { ...t, status: "todo" as const, agentId: null, sessionName: null, doneAt: null }
          : t
      );
      if (todos !== s.todos) saveTodos(todos);
      return {
        agents,
        agentChannels: channels,
        acpSessions,
        agentActiveAt: activeAt,
        agentWakeAt: wakeAt,
        hookStatus,
        agentSubmittedAt: submittedAt,
        unreadCompletion,
        agentOutputs: outputs,
        agentResources: resources,
        resumeOnOpen,
        closedAgentIds,
        todos,
      };
    }),

  updateAgentStatus: (id, status) => {
    // A running session ended naturally (`agent://exited` → done, or a failed
    // spawn marked failed): also a running → other transition. Guarded on the
    // prev status so an already-done record (replay + live event for the same
    // exit) can't re-notify.
    const prevAgent = useStore.getState().agents.get(id);
    if (
      (status === "done" || status === "failed") &&
      prevAgent &&
      (prevAgent.status === "running" || prevAgent.status === "busy")
    ) {
      notifyAgentTransition(id);
    }
    return set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, status });
      return { agents };
    });
  },

  updateAgentUsage: (id, usage) =>
    set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, last_usage: usage });
      return { agents };
    }),

  updateAgentTitle: (id, title) =>
    set((s) => {
      const agents = new Map(s.agents);
      const a = agents.get(id);
      if (a) agents.set(id, { ...a, title });
      // Keep the tab's title snapshot in lockstep so the tab bar label stays
      // consistent even after the agent record is removed (or while a stale tab
      // lingers during close).
      const tabs = s.tabs.map((t) => (t.agentId === id ? { ...t, title } : t));
      return { agents, tabs };
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
      // Output = liveness: stamp the activity clock so a streaming session
      // reads as 运行中. Throttled to ~0.5/s and reference-preserving — a fresh
      // Map (and the resulting tab strip re-render) per chunk would be too hot.
      // While the session is "waking" (freshly spawned/resumed, see addAgent),
      // its output is the CLI's welcome screen — and for TUI runtimes, idle
      // status redraws — so exclude it from the stamp. The marker is only
      // cleared by user engagement (markAgentActive), after which output is
      // real task work. (With ACTIVE_WINDOW_MS = 0 the stamp no longer drives
      // 运行中, but the bookkeeping is kept for a future non-zero window.)
      const now = Date.now();
      const waking = s.agentWakeAt.has(id);
      if (!waking && now - (s.agentActiveAt.get(id) ?? 0) >= ACTIVE_STAMP_THROTTLE_MS) {
        const activeAt = new Map(s.agentActiveAt);
        activeAt.set(id, now);
        return { agentOutputs: outputs, agentActiveAt: activeAt };
      }
      return { agentOutputs: outputs };
    }),

  markAgentActive: (id) =>
    set((s) => {
      const now = Date.now();
      // User interaction ends the wake: output that follows is a real task,
      // not the CLI's welcome screen.
      const wakeAt = s.agentWakeAt.has(id) ? new Map(s.agentWakeAt) : undefined;
      if (wakeAt) wakeAt.delete(id);
      if (now - (s.agentActiveAt.get(id) ?? 0) < ACTIVE_STAMP_THROTTLE_MS) {
        return wakeAt ? { agentWakeAt: wakeAt } : {};
      }
      const activeAt = new Map(s.agentActiveAt);
      activeAt.set(id, now);
      return wakeAt
        ? { agentActiveAt: activeAt, agentWakeAt: wakeAt }
        : { agentActiveAt: activeAt };
    }),

  markAgentSubmitted: (id) =>
    set((s) => {
      const now = Date.now();
      const submittedAt = new Map(s.agentSubmittedAt);
      submittedAt.set(id, now);
      // Mirror markAgentActive: user interaction ends the wake window.
      const wakeAt = s.agentWakeAt.has(id) ? new Map(s.agentWakeAt) : undefined;
      if (wakeAt) wakeAt.delete(id);
      // Also stamp the activity clock so the post-submit output stream keeps
      // the tab 运行中 while the hook is still catching up.
      const activeAt = new Map(s.agentActiveAt);
      activeAt.set(id, now);
      return wakeAt
        ? { agentSubmittedAt: submittedAt, agentActiveAt: activeAt, agentWakeAt: wakeAt }
        : { agentSubmittedAt: submittedAt, agentActiveAt: activeAt };
    }),

  setHookStatus: (id, hook) => {
    // Running → other transition (the hook went working → idle / waiting_input
    // / awaiting_choice / dormant): sound the completion chime and flash the
    // agent's tab label. Checked before `set` so the two callers (the 1s poll
    // and the `agent://hook-status` event) can't double-fire: the second call
    // sees prev already on the new status.
    const prevHook = useStore.getState().hookStatus.get(id);
    if (
      prevHook &&
      hook &&
      prevHook.status === "working" &&
      hook.status !== "working"
    ) {
      notifyAgentTransition(id);
    }
    return set((s) => {
      // Value-preserving: `invoke` parses a fresh object each poll, so compare
      // by value — an unchanged status must not re-render the tab strip (an
      // idle strip doesn't tick).
      const prev = s.hookStatus.get(id);
      if (
        prev &&
        hook &&
        prev.status === hook.status &&
        prev.ts === hook.ts
      ) {
        return {};
      }
      if (!hook && prev === undefined) return {};
      const hookStatus = new Map(s.hookStatus);
      if (hook) hookStatus.set(id, hook);
      else hookStatus.delete(id);
      // A turn completed: the hook went working → idle (claude Stop, codex Stop,
      // opencode session idle). If the agent's tab is off-screen the user hasn't
      // seen the result — flag it so the tab reads 已完成 instead of 空闲.
      let unreadCompletion = s.unreadCompletion;
      const tab = s.tabs.find((t) => t.agentId === id);
      if (
        hook &&
        prev &&
        prev.status === "working" &&
        hook.status === "idle" &&
        tab &&
        !tabIsVisible(tab.id, s.activeTabId, s.splitTree)
      ) {
        unreadCompletion = new Set(unreadCompletion);
        unreadCompletion.add(id);
      }
      // Task auto-complete: an in-flight (assigned) tag on this session is done
      // when the turn ends — move it to 待验收 with the session name. Unlike the
      // unread flag this does not depend on tab visibility; a finished task is
      // finished whether or not anyone was watching the terminal.
      let todos = s.todos;
      if (hook && prev && prev.status === "working" && hook.status === "idle") {
        const sessionName = s.agents.get(id)?.title;
        if (todos.some((t) => t.status === "assigned" && t.agentId === id)) {
          todos = todos.map((t) =>
            t.status === "assigned" && t.agentId === id
              ? { ...t, status: "done" as const, doneAt: Date.now(), sessionName: sessionName ?? t.sessionName }
              : t
          );
          saveTodos(todos);
        }
      }
      return { hookStatus, unreadCompletion, todos };
    });
  },

  flashTab: (id) =>
    set((s) => {
      const tabFlash = new Map(s.tabFlash);
      tabFlash.set(id, (s.tabFlash.get(id) ?? 0) + 1);
      return { tabFlash };
    }),

  setAgentUnread: (id, unread) =>
    set((s) => {
      const has = s.unreadCompletion.has(id);
      if (has === unread) return {};
      const unreadCompletion = new Set(s.unreadCompletion);
      if (unread) unreadCompletion.add(id);
      else unreadCompletion.delete(id);
      return { unreadCompletion };
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
      const hasPty = s.agentChannels.has(id);
      const hasAcp = s.acpSessions.has(id);
      if (!hasPty && !hasAcp) return {};
      const agentChannels = hasPty
        ? (() => {
            const m = new Map(s.agentChannels);
            m.delete(id);
            return m;
          })()
        : s.agentChannels;
      // Keep transcript; mark host not live so status reads dormant until resume.
      let acpSessions = s.acpSessions;
      if (hasAcp) {
        acpSessions = new Map(s.acpSessions);
        const prev = acpSessions.get(id);
        if (prev) acpSessions.set(id, { ...prev, live: false, turnActive: false });
      }
      return { agentChannels, acpSessions };
    }),

  setSessionsRestored: () => set({ sessionsRestored: true }),

  setRuntimes: (runtimes) => set({ runtimes }),

  setUsage: (runtime, usage) =>
    set((s) => ({ usageState: { ...s.usageState, [runtime]: usage } })),
  bumpUsageRevision: () => set((s) => ({ usageRevision: s.usageRevision + 1 })),

  addTab: (tab) =>
    set((s) => {
      const tabs = [...s.tabs.filter((t) => t.id !== tab.id), tab];
      // With an active split, surface a newly opened tab in the first pane.
      const tree = s.splitTree;
      if (tree && !splitLeafTabIds(tree).includes(tab.id)) {
        return {
          tabs,
          activeTabId: tab.id,
          splitTree: splitReplaceLeaf(tree, splitFirstLeaf(tree), tab.id),
        };
      }
      return { tabs, activeTabId: tab.id };
    }),

  addTabSilent: (tab) =>
    set((s) => ({
      tabs: [...s.tabs.filter((t) => t.id !== tab.id), tab],
    })),

  reorderTabs: (fromIndex, toIndex) =>
    set((s) => {
      if (fromIndex === toIndex) return {};
      const n = s.tabs.length;
      if (fromIndex < 0 || toIndex < 0 || fromIndex >= n || toIndex >= n) return {};
      const tabs = [...s.tabs];
      const [moved] = tabs.splice(fromIndex, 1);
      tabs.splice(toIndex, 0, moved);
      return { tabs };
    }),

  closeTab: (id) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== id);
      // Closing a tab that occupies a split leaf prunes that leaf and collapses
      // its parent split to the surviving sibling.
      const tree = s.splitTree;
      if (tree && splitLeafTabIds(tree).includes(id)) {
        const pruned = splitRemoveLeaf(tree, id);
        if (!pruned) {
          // The last visible tab was closed → back to the (possibly empty)
          // single-panel view.
          return {
            tabs,
            activeTabId: tabs[tabs.length - 1]?.id ?? null,
            splitTree: null,
          };
        }
        if (splitLeafCount(pruned) === 1) {
          // One pane left → collapse to the single-panel view (keeps the
          // OpenCode resident-terminal handling).
          return {
            tabs,
            activeTabId:
              s.activeTabId === id ? splitFirstLeaf(pruned) : s.activeTabId,
            splitTree: null,
          };
        }
        const visible = splitLeafTabIds(pruned);
        return {
          tabs,
          activeTabId:
            s.activeTabId !== null && visible.includes(s.activeTabId)
              ? s.activeTabId
              : splitFirstLeaf(pruned),
          splitTree: pruned,
        };
      }
      const activeTabId =
        s.activeTabId === id ? (tabs[tabs.length - 1]?.id ?? null) : s.activeTabId;
      return { tabs, activeTabId };
    }),

  setActiveTab: (id) =>
    set((s) => {
      // With an active split, clicking a tab that isn't already visible focuses
      // it in the first pane.
      const tree = s.splitTree;
      let splitTree = tree;
      if (tree && !splitLeafTabIds(tree).includes(id)) {
        splitTree = splitReplaceLeaf(tree, splitFirstLeaf(tree), id);
      }
      // Viewing a tab clears its agent's unviewed-completion flag (已完成 → 空闲).
      let unreadCompletion = s.unreadCompletion;
      const tab = s.tabs.find((t) => t.id === id);
      if (tab?.agentId && unreadCompletion.has(tab.agentId)) {
        unreadCompletion = new Set(unreadCompletion);
        unreadCompletion.delete(tab.agentId);
      }
      return { activeTabId: id, splitTree, unreadCompletion };
    }),

  splitPane: (targetTabId, newTabId, direction, newOnFirst) =>
    set((s) => {
      const fresh: SplitNode = { kind: "leaf", id: splitUid(), tabId: newTabId };
      if (!s.splitTree) {
        const existing: SplitNode = { kind: "leaf", id: splitUid(), tabId: targetTabId };
        return {
          splitTree: {
            kind: "split",
            id: splitUid(),
            direction,
            ratio: 0.5,
            a: newOnFirst ? fresh : existing,
            b: newOnFirst ? existing : fresh,
          },
        };
      }
      return {
        splitTree: splitInsert(s.splitTree, targetTabId, newTabId, direction, newOnFirst),
      };
    }),

  setSplitTree: (tree) => set({ splitTree: tree }),

  setSplitRatio: (nodeId, ratio) =>
    set((s) => {
      if (!s.splitTree) return {};
      return {
        splitTree: splitSetRatio(
          s.splitTree,
          nodeId,
          Math.max(0.2, Math.min(0.8, ratio))
        ),
      };
    }),

  setDraggedTabId: (id) => set({ draggedTabId: id }),

  toggleComposer: () => set((s) => ({ composerOpen: !s.composerOpen })),

  requestFocus: (target) =>
    set((s) => ({
      focusRequest: { target, seq: (s.focusRequest?.seq ?? 0) + 1 },
    })),

  requestSearch: (target) =>
    set((s) => ({
      searchRequest: { target, seq: (s.searchRequest?.seq ?? 0) + 1 },
    })),

  requestReveal: (filePath, line, column) =>
    set((s) => ({
      revealRequest: { filePath, line, column, seq: (s.revealRequest?.seq ?? 0) + 1 },
    })),

  updateFileSearch: (root, patch) =>
    set((s) => ({
      fileSearchByRoot: {
        ...s.fileSearchByRoot,
        [root]: {
          ...(s.fileSearchByRoot[root] ?? defaultFileSearchState()),
          ...patch,
        },
      },
    })),

  clearFileSearch: (root) =>
    set((s) => {
      if (!(root in s.fileSearchByRoot)) return {};
      const next = { ...s.fileSearchByRoot };
      delete next[root];
      return { fileSearchByRoot: next };
    }),

  setPermissionMode: (mode) => set({ permissionMode: mode }),

  setSpeed: (speed) => set({ speed }),

  setSelectedModel: (model) => set({ selectedModel: model }),

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
  setProjects: (projects) => set({ projects }),

  setProjectRoots: (roots) =>
    set((s) => ({ projectRoots: { ...s.projectRoots, ...roots } })),

  setFocusedProject: (name) => {
    set({ focusedProject: name });
    // Remember the last focused project across restarts. Empty string clears
    // a stale value when focus is dropped (e.g. the focused project was removed).
    invoke("setting_set", {
      key: "focused_project",
      value: name ?? "",
    }).catch(() => {});
  },

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

  beginClone: (id, name) =>
    set((s) => ({ pendingClones: { ...s.pendingClones, [id]: name } })),

  finishClone: (id) =>
    set((s) => {
      if (!(id in s.pendingClones)) return {};
      const pendingClones = { ...s.pendingClones };
      delete pendingClones[id];
      return { pendingClones };
    }),

  removeProject: (name) => {
    dropProjectLocal(name);
    // Delete the project's workspace dir (sessions / agent metadata / context).
    // Custom-rooted projects only lose this metadata dir — the real folder
    // (picked / cloned) is never touched by the backend command.
    invoke("delete_project", { name }).catch(() => {});
  },

  setWorktrees: (list) => set({ worktrees: list }),

  addWorktree: (meta, name) => {
    set((s) => ({
      worktrees: s.worktrees.some((w) => w.path === meta.path)
        ? s.worktrees
        : [...s.worktrees, meta],
    }));
    get().addProject(name, meta.path);
  },

  removeWorktreeLocal: (path) => {
    const s = get();
    const name = Object.entries(s.projectRoots).find(
      ([, root]) => root === path
    )?.[0];
    if (name) dropProjectLocal(name);
    set((st) => ({ worktrees: st.worktrees.filter((w) => w.path !== path) }));
  },

  removeWorktree: (path) => {
    get().removeWorktreeLocal(path);
    invoke("worktree_remove", { path }).catch(() => {});
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

  // Sleep a project (free CPU/memory): kill every agent's PTY/ACP process, close its
  // terminal + editor/diff tabs. The agent stays in the store as idle (reopening
  // the terminal resumes the session) and the DB rows persist for restart.
  sleepProject: (name) => {
    const s = get();
    const root = s.projectRoots[name];
    const doomed: string[] = [];
    s.agents.forEach((a, id) => {
      if (projectOfAgent(a) === name) doomed.push(id);
    });
    const channels = new Map(s.agentChannels);
    const acpSessions = new Map(s.acpSessions);
    for (const id of doomed) {
      invoke("agent_kill", { id }).catch(() => {});
      s.closeTab(id);
      channels.delete(id);
      const prev = acpSessions.get(id);
      if (prev) {
        acpSessions.set(id, { ...prev, live: false, turnActive: false });
      }
    }
    set({ agentChannels: channels, acpSessions });
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
    // Keep the persisted last-focus key in sync when the focused project is
    // renamed, otherwise the next startup restores the old name and falls back.
    if (s.focusedProject === oldName) {
      invoke("setting_set", {
        key: "focused_project",
        value: newName,
      }).catch(() => {});
    }
    return newName;
  },

  applyResourceSample: (resources) =>
    set((s) => {
      if (resources.length === 0) return {};
      const agentResources = new Map(s.agentResources);
      for (const r of resources) {
        const point: ResourcePoint = { cpu_pct: r.cpu_pct, mem_bytes: r.mem_bytes };
        agentResources.set(r.agent_id, point);
      }
      return { agentResources };
    }),

  setOnboarded: (onboarded) => {
    try {
      localStorage.setItem(ONBOARDED_KEY, onboarded ? "1" : "0");
    } catch {
      // ignore storage errors
    }
    set({ onboarded });
  },

  // New-project modal visibility is global (opened from the sidebar "+", the
  // sidebar empty row, and the main-area empty state), so it lives in the store.
  setNprojOpen: (open) => set({ nprojOpen: open }),

  setFontScale: (scale) => {
    try {
      localStorage.setItem(FONT_SCALE_KEY, scale);
    } catch {
      // ignore storage errors
    }
    set({ fontScale: scale });
  },

  setThemeId: (themeId) => {
    try {
      localStorage.setItem(THEME_KEY, themeId);
    } catch {
      // ignore storage errors
    }
    set({ themeId });
  },

  setCtrlTRuntime: (runtime) => {
    try {
      localStorage.setItem(CTRLT_RUNTIME_KEY, runtime);
    } catch {
      // ignore storage errors
    }
    set({ ctrlTRuntime: runtime });
  },

  // ── App self-update ───────────────────────────────────────────
  // Only the auto-check toggle persists through the store; the rest of the
  // slice is written imperatively by `ui/state/update.ts` via setState.

  setAutoCheckUpdate: (enabled) => {
    set({ autoCheckUpdate: enabled });
    invoke("setting_set", {
      key: "auto_check_update",
      value: enabled ? "true" : "false",
    }).catch(() => {});
  },

  setSoundEnabled: (enabled) => {
    set({ soundEnabled: enabled });
    invoke("setting_set", {
      key: "sound_enabled",
      value: enabled ? "true" : "false",
    }).catch(() => {});
  },

  pollCiStatus: () => {
    const s = useStore.getState();
    if (s.ciPolling) return;
    set({ ciPolling: true });
    invoke<CiStatus>("ci_status")
      .then((status) => set({ ciStatus: status }))
      .catch((e) => set({ ciStatus: { tag: "", run: null, error: String(e) } }))
      .finally(() => set({ ciPolling: false }));
  },

  // ── Todo tags ─────────────────────────────────────────────────
  // All mutating actions persist to the settings KV (`saveTodos`). Hydration
  // (`setTodos`) is the exception — TodoPanel writes it back only when it
  // repaired orphans, not on every mount.

  setTodos: (todos) => set({ todos }),

  addTodo: (text) =>
    set((s) => {
      const t = text.trim();
      if (!t) return {};
      // A tag created in project scope belongs to the focused project; global
      // scope keeps it global.
      const project = s.todoScope === "project" ? s.focusedProject : null;
      const tag: TodoTag = {
        id: todoUid(),
        text: t,
        status: "todo",
        agentId: null,
        sessionName: null,
        project,
        createdAt: Date.now(),
        doneAt: null,
      };
      const todos = [...s.todos, tag];
      saveTodos(todos);
      return { todos };
    }),

  updateTodoText: (id, text) =>
    set((s) => {
      const trimmed = text.trim();
      if (!trimmed) return {};
      const todos = s.todos.map((t) => (t.id === id ? { ...t, text: trimmed } : t));
      saveTodos(todos);
      return { todos };
    }),

  assignTodoToAgent: (id, agentId, sessionName) =>
    set((s) => {
      // The tag keeps its creation-time scope: only the assignment link changes.
      // A tag born in the global view stays global (project untouched) so it
      // lands back in the global 待验收 when the session's turn ends — re-homing
      // it to the session's project here would make it vanish from the view it
      // was created in.
      // A session never accumulates multiple task tags: if it already has one
      // (assigned in-flight or a 待验收 tag), the new assignment supersedes it —
      // drop the session's existing tag(s) so only the newest one remains.
      const todos = s.todos
        .filter((t) => t.id !== id && t.agentId !== agentId)
        .map((t) =>
          t.id === id && t.status === "todo"
            ? { ...t, status: "assigned" as const, agentId, sessionName }
            : t
        );
      saveTodos(todos);
      return { todos };
    }),

  deleteTodo: (id) =>
    set((s) => {
      const todos = s.todos.filter((t) => t.id !== id);
      saveTodos(todos);
      return { todos };
    }),

  toggleTodoScope: () =>
    set((s) => ({
      todoScope: s.todoScope === "global" ? "project" : "global",
    })),
  };
});

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

/** Fire the running → other transition notification: play the theme-mapped
 *  confirmation chime and request a two-flash on the agent's tab label. */
function notifyAgentTransition(id: string): void {
  const s = useStore.getState();
  if (s.soundEnabled) playConfirmationSound(s.themeId);
  s.flashTab(id);
}
