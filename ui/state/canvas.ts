import { useStore, type AgentInfo, type Tab } from "./store";
import { t } from "../i18n";

export interface CanvasScope {
  projectId: string;
  workspaceId: string;
}

export function canvasTabId(scope: CanvasScope): string {
  return `canvas:${scope.workspaceId}`;
}

export function canvasTab(scope: CanvasScope): Tab {
  return {
    id: canvasTabId(scope),
    type: "canvas",
    title: t("canvas.tabTitle", { project: scope.projectId }),
    project: scope.projectId,
    filePath: scope.workspaceId,
  };
}

function projectOfCwd(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Current canvas scope: focusedProject → active tab's project → "default". */
export function resolveCanvasScope(): CanvasScope {
  const s = useStore.getState();
  let projectId: string | null | undefined = s.focusedProject;
  if (!projectId) {
    const tab = s.tabs.find((tb) => tb.id === s.activeTabId);
    if (tab?.type === "canvas" && tab.project) {
      projectId = tab.project;
    } else if (tab?.type === "agent" && tab.agentId) {
      const agent = s.agents.get(tab.agentId);
      projectId =
        agent?.project ?? (agent?.cwd ? projectOfCwd(agent.cwd) : undefined);
    } else if (tab?.project) {
      projectId = tab.project;
    } else if (tab?.filePath) {
      let best: string | undefined;
      let bestLen = -1;
      for (const [name, root] of Object.entries(s.projectRoots)) {
        const prefix = root.endsWith("/") ? root : `${root}/`;
        if (tab.filePath.startsWith(prefix) && prefix.length > bestLen) {
          best = name;
          bestLen = prefix.length;
        }
      }
      projectId = best;
    }
  }
  projectId = projectId ?? "default";
  const root = s.projectRoots[projectId];
  const wt = root ? s.worktrees.find((w) => w.path === root) : undefined;
  const workspaceId = wt?.path ?? root ?? projectId;
  return { projectId, workspaceId };
}

const returnTabByScope = new Map<string, string>();

export function rememberCanvasReturnTab(
  scope: CanvasScope,
  tabId: string | null
): void {
  if (!tabId) return;
  const s = useStore.getState();
  const tab = s.tabs.find((tb) => tb.id === tabId);
  if (!tab || tab.type === "canvas") return;
  returnTabByScope.set(canvasTabId(scope), tabId);
}

export function takeCanvasReturnTab(scope: CanvasScope): string | null {
  const key = canvasTabId(scope);
  const id = returnTabByScope.get(key) ?? null;
  if (!id) return null;
  const s = useStore.getState();
  if (!s.tabs.some((tb) => tb.id === id)) {
    returnTabByScope.delete(key);
    return null;
  }
  return id;
}

/** Ensure every project has a canvas tab (silent — does not steal focus). */
export function ensureProjectCanvasTabs(): void {
  const s0 = useStore.getState();
  for (const projectId of s0.projects) {
    const root = s0.projectRoots[projectId];
    const wt = root ? s0.worktrees.find((w) => w.path === root) : undefined;
    const workspaceId = wt?.path ?? root ?? projectId;
    const tab = canvasTab({ projectId, workspaceId });
    const s = useStore.getState();
    if (!s.tabs.some((tb) => tb.id === tab.id)) {
      s.addTabSilent(tab);
    }
  }
}

/** Open (or activate) the canvas tab for `scope`. */
export function openCanvas(scope?: CanvasScope): string {
  ensureProjectCanvasTabs();
  const s = useStore.getState();
  const resolved = scope ?? resolveCanvasScope();
  const id = canvasTabId(resolved);
  if (s.tabs.some((tb) => tb.id === id)) {
    s.setActiveTab(id);
    return id;
  }
  s.addTab(canvasTab(resolved));
  return id;
}

/**
 * TabBar switch: canvas tab showing → back to the last non-canvas tab;
 * otherwise open canvases for every project and focus the current scope.
 */
export function toggleCanvasView(): void {
  const s = useStore.getState();
  const current = s.tabs.find((tb) => tb.id === s.activeTabId);
  if (current?.type === "canvas") {
    const scope: CanvasScope = {
      projectId: current.project ?? "default",
      workspaceId: current.filePath ?? current.project ?? "default",
    };
    const prev = takeCanvasReturnTab(scope);
    if (prev) {
      s.setActiveTab(prev);
      return;
    }
    const fallback = s.tabs.find((tb) => tb.type !== "canvas");
    if (fallback) s.setActiveTab(fallback.id);
    return;
  }
  const scope = resolveCanvasScope();
  rememberCanvasReturnTab(scope, s.activeTabId);
  openCanvas(scope);
}

type CanvasCenterReq = { agentId: string; seq: number };
let canvasCenterReq: CanvasCenterReq = { agentId: "", seq: 0 };
const canvasCenterListeners = new Set<() => void>();

/** Ask the open canvas to pan so `agentId`'s card is centered. */
export function requestCanvasCenter(agentId: string): void {
  canvasCenterReq = { agentId, seq: canvasCenterReq.seq + 1 };
  for (const l of canvasCenterListeners) l();
}

export function subscribeCanvasCenter(fn: () => void): () => void {
  canvasCenterListeners.add(fn);
  return () => {
    canvasCenterListeners.delete(fn);
  };
}

export function getCanvasCenterReq(): CanvasCenterReq {
  return canvasCenterReq;
}

/** Open this project's canvas and center the given session card. */
export function revealAgentOnCanvas(projectId: string, agentId: string): void {
  const s = useStore.getState();
  s.setFocusedProject(projectId);
  const workspaceId = s.projectRoots[projectId] ?? projectId;
  openCanvas({ projectId, workspaceId });
  requestCanvasCenter(agentId);
}

// ── Graph types (camelCase; matches Rust serde rename_all) ─────

export interface CanvasVec {
  x: number;
  y: number;
}

export interface CanvasSize {
  w: number;
  h: number;
}

export interface CanvasViewport {
  x: number;
  y: number;
  zoom: number;
}

export interface CanvasTerminal {
  id: string;
  name: string;
  cwd: string;
  command: string;
  kind: "task" | "service" | string;
  agentId: string | null;
  position: CanvasVec;
  size: CanvasSize;
  portPolicy?: string | null;
  port?: number | null;
  readyPattern?: string | null;
}

export interface CanvasAgentLayout {
  id: string;
  position: CanvasVec;
  size: CanvasSize;
}

export interface CanvasEdge {
  id: string;
  source: string;
  target: string;
}

export interface CanvasCombination {
  id: string;
  memberTerminalIds: string[];
}

export interface BlockGraph {
  version: number;
  projectId: string;
  workspaceId: string;
  viewport: CanvasViewport;
  terminals: CanvasTerminal[];
  edges: CanvasEdge[];
  combinations: CanvasCombination[];
  agents: CanvasAgentLayout[];
  agentsHidden?: string[];
}

export const DEFAULT_CARD_SIZE: CanvasSize = { w: 240, h: 88 };

export function emptyBlockGraph(scope: CanvasScope): BlockGraph {
  return {
    version: 1,
    projectId: scope.projectId,
    workspaceId: scope.workspaceId,
    viewport: { x: 0, y: 0, zoom: 1 },
    terminals: [],
    edges: [],
    combinations: [],
    agents: [],
    agentsHidden: [],
  };
}

export const CANVAS_AGENT_DRAG_MIME = "application/x-capilot-canvas-agent";

let activeCanvasAgentDragId: string | null = null;

export function beginCanvasAgentDrag(agentId: string): void {
  activeCanvasAgentDragId = agentId;
}

export function endCanvasAgentDrag(): void {
  activeCanvasAgentDragId = null;
}

export function isCanvasAgentDrag(dt: DataTransfer | null | undefined): boolean {
  if (activeCanvasAgentDragId) return true;
  if (!dt) return false;
  try {
    return Array.from(dt.types as unknown as ArrayLike<string>).includes(
      CANVAS_AGENT_DRAG_MIME
    );
  } catch {
    return false;
  }
}

export function getCanvasAgentDragId(
  dt: DataTransfer | null | undefined
): string | null {
  if (activeCanvasAgentDragId) return activeCanvasAgentDragId;
  if (!dt) return null;
  try {
    return dt.getData(CANVAS_AGENT_DRAG_MIME) || dt.getData("text/plain") || null;
  } catch {
    return null;
  }
}

function agentInScope(agent: AgentInfo, scope: CanvasScope): boolean {
  if (agent.project && agent.project === scope.projectId) return true;
  const root = scope.workspaceId;
  if (!root) return false;
  const prefix = root.endsWith("/") ? root : `${root}/`;
  return agent.cwd === root || agent.cwd.startsWith(prefix);
}

/**
 * Overlay live sessions onto a persisted graph.
 * Shell runtimes become terminal blocks; coding agents become console layouts.
 * Does not mutate `graph`. Does not persist — caller writes on user drag.
 */
export function mergeAgentsIntoGraph(
  graph: BlockGraph,
  agents: Iterable<AgentInfo>,
  scope: CanvasScope
): BlockGraph {
  const hidden = new Set(graph.agentsHidden ?? []);
  const liveIds = new Set(
    [...agents]
      .filter((a) => agentInScope(a, scope) && !hidden.has(a.id))
      .map((a) => a.id)
  );

  const terminals: CanvasTerminal[] = [];
  const consoles: CanvasAgentLayout[] = [];
  const seenTerm = new Set<string>();
  const seenConsole = new Set<string>();
  const edges: CanvasEdge[] = [];
  const seenEdge = new Set<string>();

  for (const term of graph.terminals) {
    if (term.agentId && !liveIds.has(term.agentId) && !term.agentId.startsWith("pending:")) {
      continue;
    }
    if (seenTerm.has(term.id)) continue;
    seenTerm.add(term.id);
    terminals.push(term);
  }
  for (const a of graph.agents) {
    if ((!liveIds.has(a.id) && !a.id.startsWith("pending:")) || seenConsole.has(a.id)) continue;
    seenConsole.add(a.id);
    consoles.push(a);
  }
  for (const e of graph.edges) {
    if (seenEdge.has(e.id)) continue;
    seenEdge.add(e.id);
    edges.push(e);
  }

  const liveTermIds = new Set(terminals.map((t) => t.id));
  return {
    ...graph,
    projectId: scope.projectId,
    workspaceId: scope.workspaceId,
    terminals,
    agents: consoles,
    edges: edges.filter((e) => liveTermIds.has(e.source) && liveTermIds.has(e.target)),
  };
}

/** Open or focus the agent tab for `agentId` (same as LeftSidebar.openAgentTab). */
export function focusAgentTab(agentId: string): void {
  const s = useStore.getState();
  const agent = s.agents.get(agentId);
  if (!agent) return;
  if (!s.tabs.some((tb) => tb.id === agentId)) {
    s.addTab({
      id: agentId,
      type: "agent",
      agentId,
      title: agent.title || `agent-${agentId.slice(0, 6)}`,
    });
  } else {
    s.setActiveTab(agentId);
  }
}
