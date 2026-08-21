import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore, ACTIVE_WINDOW_MS, resolveCtrlTRuntime } from "../../state/store";
import {
  type BlockGraph,
  type CanvasScope,
  type CanvasVec,
  type CanvasViewport,
  DEFAULT_CARD_SIZE,
  emptyBlockGraph,
  focusAgentTab,
  mergeAgentsIntoGraph,
  isCanvasAgentDrag,
  getCanvasAgentDragId,
  endCanvasAgentDrag,
  subscribeCanvasCenter,
  getCanvasCenterReq,
} from "../../state/canvas";
import { isShellRuntime } from "../../state/shellPath";
import { closeAgent as closeAgentAction, spawnAgent } from "../../state/agentActions";
import { notify } from "../../state/notify";
import { useT } from "../../i18n";
import { Icon } from "../Icon";
import { TerminalTemplatePicker } from "../layout/TerminalTemplatePicker";
import { CanvasNodeCard } from "./CanvasNodeCard";
import { CanvasToolbar } from "./CanvasToolbar";
import {
  getCanvasLayoutPrefs,
  subscribeCanvasLayoutPrefs,
} from "../../state/canvasLayout";

interface RunStatus {
  runId: string;
  nodeStates: Record<string, string>;
  blocked: string[];
  ready: string[];
  leases?: Record<string, number>;
}

const MIN_ZOOM = 0.25;
const MAX_ZOOM = 2;
const SAVE_DEBOUNCE_MS = 400;
const DRAG_THRESHOLD = 5;
const EXPANDED_MIN = { w: 700, h: 700 };

function clamp(n: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, n));
}

function screenToWorld(
  clientX: number,
  clientY: number,
  surface: DOMRect,
  vp: CanvasViewport
): CanvasVec {
  const z = vp.zoom || 1;
  return {
    x: (clientX - surface.left - vp.x) / z,
    y: (clientY - surface.top - vp.y) / z,
  };
}

function worldToSurface(pos: CanvasVec, vp: CanvasViewport): CanvasVec {
  return {
    x: vp.x + pos.x * vp.zoom,
    y: vp.y + pos.y * vp.zoom,
  };
}

function findFreeWorldPos(
  _preferred: CanvasVec,
  occupied: { pos: CanvasVec; size: { w: number; h: number } }[],
  size: { w: number; h: number },
  gap = 24
): CanvasVec {
  if (occupied.length === 0) return { x: 80, y: 80 };

  const cx = (o: { pos: CanvasVec; size: { w: number; h: number } }) => o.pos.x + o.size.w / 2;
  const cy = (o: { pos: CanvasVec; size: { w: number; h: number } }) => o.pos.y + o.size.h / 2;
  const sep = (
    a: { pos: CanvasVec; size: { w: number; h: number } },
    b: { pos: CanvasVec; size: { w: number; h: number } }
  ) => {
    const dx = Math.abs(cx(a) - cx(b)) - (a.size.w + b.size.w) / 2;
    const dy = Math.abs(cy(a) - cy(b)) - (a.size.h + b.size.h) / 2;
    if (dx <= 0 && dy <= 0) return -Math.hypot(Math.min(0, dx), Math.min(0, dy));
    if (dx <= 0) return dy;
    if (dy <= 0) return dx;
    return Math.hypot(dx, dy);
  };
  const fits = (pos: CanvasVec) => {
    const me = { pos, size };
    return occupied.every((o) => sep(me, o) >= gap - 0.5);
  };

  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  let bcx = 0, bcy = 0;
  for (const o of occupied) {
    minX = Math.min(minX, o.pos.x);
    minY = Math.min(minY, o.pos.y);
    maxX = Math.max(maxX, o.pos.x + o.size.w);
    maxY = Math.max(maxY, o.pos.y + o.size.h);
    bcx += cx(o);
    bcy += cy(o);
  }
  bcx /= occupied.length;
  bcy /= occupied.length;

  const origin = { x: bcx - size.w / 2, y: bcy - size.h / 2 };
  if (fits(origin)) return origin;

  const step = gap + 8;
  const maxR = Math.max(size.w, size.h) * 8 + gap * 12;
  let best: { pos: CanvasVec; d: number; r: number } | null = null;
  for (let r = step; r <= maxR; r += step) {
    const n = Math.max(12, Math.round((2 * Math.PI * r) / step));
    for (let i = 0; i < n; i++) {
      const ang = (i / n) * Math.PI * 2 - Math.PI / 2;
      const pos = {
        x: origin.x + Math.cos(ang) * r,
        y: origin.y + Math.sin(ang) * r,
      };
      if (!fits(pos)) continue;
      const d = (pos.x + size.w / 2 - bcx) ** 2 + (pos.y + size.h / 2 - bcy) ** 2;
      if (!best || r < best.r - 0.5 || (Math.abs(r - best.r) < 0.5 && d < best.d)) {
        best = { pos, d, r };
      }
    }
    if (best && best.r <= r) break;
  }
  return best?.pos ?? { x: maxX + gap, y: minY };
}

function cardOnScreen(
  pos: CanvasVec,
  size: { w: number; h: number },
  vp: CanvasViewport,
  view: { w: number; h: number },
  pad = 80
): boolean {
  const z = vp.zoom || 1;
  const x = vp.x + pos.x * z;
  const y = vp.y + pos.y * z;
  const w = size.w * z;
  const h = size.h * z;
  return x + w > -pad && y + h > -pad && x < view.w + pad && y < view.h + pad;
}

/** Critically-damped spring matching cleancode's presenceCreateSpringDynamics
 *  (dampingRatio 1, response 0.34s): scale 0→1 from the card center. */
function CanvasAppear({
  kind,
  children,
}: {
  kind?: "create" | "drop";
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !kind) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      el.style.setProperty("--canvas-appear-scale", "1");
      el.style.setProperty("--canvas-appear-opacity", "1");
      return;
    }
    const response = kind === "create" ? 0.34 : 0.24;
    const wn = (2 * Math.PI) / response;
    let scale = kind === "create" ? 0 : 0.86;
    let opacity = kind === "create" ? 0.16 : 0;
    let vScale = 0;
    let vOp = 0;
    let last = performance.now();
    let raf = 0;
    const step = (value: number, vel: number, dt: number) => {
      const displacement = value - 1;
      const velocityTerm = vel + wn * displacement;
      const decay = Math.exp(-wn * dt);
      return {
        value: 1 + (displacement + velocityTerm * dt) * decay,
        velocity: (vel - velocityTerm * wn * dt) * decay,
      };
    };
    el.style.setProperty("--canvas-appear-scale", String(scale));
    el.style.setProperty("--canvas-appear-opacity", String(opacity));
    const tick = (now: number) => {
      const dt = Math.min(1 / 30, Math.max(0, (now - last) / 1000));
      last = now;
      const s = step(scale, vScale, dt);
      const o = step(opacity, vOp, dt);
      scale = s.value;
      vScale = s.velocity;
      opacity = o.value;
      vOp = o.velocity;
      el.style.setProperty("--canvas-appear-scale", String(scale));
      el.style.setProperty("--canvas-appear-opacity", String(opacity));
      if (
        Math.abs(scale - 1) <= 0.002 &&
        Math.abs(opacity - 1) <= 0.002 &&
        Math.abs(vScale) <= 0.02 &&
        Math.abs(vOp) <= 0.02
      ) {
        el.style.setProperty("--canvas-appear-scale", "1");
        el.style.setProperty("--canvas-appear-opacity", "1");
        return;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [kind]);
  return (
    <div
      ref={ref}
      className={kind ? `canvas-node-appear canvas-node-appear-${kind}` : "canvas-node-appear"}
    >
      {children}
    </div>
  );
}

function EdgeLine({
  x1,
  y1,
  x2,
  y2,
  selected,
  preview,
  onPointerDown,
}: {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  selected?: boolean;
  preview?: boolean;
  onPointerDown?: (e: React.PointerEvent<HTMLDivElement>) => void;
}) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const len = Math.hypot(dx, dy);
  if (len < 0.5) return null;
  const ang = Math.atan2(dy, dx);
  return (
    <div
      className={`canvas-edge-html${selected ? " selected" : ""}${preview ? " preview" : ""}`}
      style={{
        left: x1,
        top: y1,
        width: len,
        transform: `rotate(${ang}rad)`,
      }}
      onPointerDown={onPointerDown}
    />
  );
}

export function CanvasPanel({
  scope,
  active: _active,
}: {
  scope: CanvasScope;
  active?: boolean;
}) {
  const t = useT();
  const [layout, setLayout] = useState(getCanvasLayoutPrefs);
  useEffect(() => subscribeCanvasLayoutPrefs(() => setLayout(getCanvasLayoutPrefs())), []);
  const CARD = { w: layout.cardW, h: layout.cardH };
  const agentIdSig = useStore((s) => [...s.agents.keys()].sort().join("\0"));
  const agentActiveAt = useStore((s) => s.agentActiveAt);

  const [graph, setGraph] = useState<BlockGraph | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [viewport, setViewport] = useState<CanvasViewport>({ x: 0, y: 0, zoom: 1 });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedTermId, setSelectedTermId] = useState<string | null>(null);
  const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
  const [expandedSizes, setExpandedSizes] = useState<Record<string, { w: number; h: number }>>(
    {}
  );
  const [run, setRun] = useState<RunStatus | null>(null);
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; world: CanvasVec } | null>(null);
  const [cardMenu, setCardMenu] = useState<{
    x: number;
    y: number;
    agentId: string;
    kind: "terminal" | "console";
    nodeId: string;
    runtime: string;
  } | null>(null);
  const [termPicker, setTermPicker] = useState<{ x: number; y: number; world: CanvasVec } | null>(null);
  const pendingAppearRef = useRef<Record<string, "create" | "drop">>({});
  const worldRef = useRef<HTMLDivElement>(null);
  const camRafRef = useRef<number | null>(null);
  const [connectPreview, setConnectPreview] = useState<{
    x1: number;
    y1: number;
    x2: number;
    y2: number;
  } | null>(null);
  const [cardDragging, setCardDragging] = useState(false);

  const launchedRef = useRef<Set<string>>(new Set());
  const reportedRef = useRef<Set<string>>(new Set());
  const didDragRef = useRef(false);
  const lastClickRef = useRef<{ id: string; at: number } | null>(null);
  const pendingPosRef = useRef<Map<string, CanvasVec>>(new Map());
  const [, setTick] = useState(0);

  const viewportRef = useRef(viewport);
  viewportRef.current = viewport;
  const graphRef = useRef(graph);
  graphRef.current = graph;
  const mergedRef = useRef<BlockGraph | null>(null);
  const surfaceRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (camRafRef.current != null) return;
    const el = worldRef.current;
    if (!el) return;
    const z = viewport.zoom || 1;
    el.style.transform = `translate(${viewport.x}px, ${viewport.y}px) scale(${z})`;
  }, [viewport]);
  const [viewSize, setViewSize] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const el = surfaceRef.current;
    if (!el) return;
    const measure = () => setViewSize({ w: el.clientWidth, h: el.clientHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const saveTimer = useRef<number | null>(null);
  const panRef = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    orig: CanvasViewport;
  } | null>(null);
  const dragRef = useRef<{
    pointerId: number;
    kind: "terminal" | "console";
    id: string;
    start: CanvasVec;
    orig: CanvasVec;
    armed: boolean;
  } | null>(null);
  const connectRef = useRef<{
    pointerId: number;
    fromId: string;
    from: CanvasVec;
  } | null>(null);
  const resizeRef = useRef<{
    pointerId: number;
    agentId: string;
    start: CanvasVec;
    orig: { w: number; h: number };
  } | null>(null);
  const centerSeqRef = useRef(0);

  const persist = useCallback(
    (next: BlockGraph, vp: CanvasViewport) => {
      const payload: BlockGraph = {
        ...next,
        viewport: vp,
        projectId: scope.projectId,
        workspaceId: scope.workspaceId,
      };
      if (saveTimer.current != null) window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(() => {
        saveTimer.current = null;
        void invoke("canvas_graph_set", {
          project: scope.projectId,
          workspaceId: scope.workspaceId,
          graph: payload,
        }).catch((e) => {
          notify(t("canvas.loadFailed"), typeof e === "string" ? e : String(e));
        });
      }, SAVE_DEBOUNCE_MS);
    },
    [scope.projectId, scope.workspaceId, t]
  );

  useEffect(() => {
    let cancelled = false;
    setGraph(null);
    setLoadError(null);
    invoke<BlockGraph>("canvas_graph_get", {
      project: scope.projectId,
      workspaceId: scope.workspaceId,
    })
      .then((g) => {
        if (cancelled) return;
        setGraph(g);
        setViewport(g.viewport ?? { x: 0, y: 0, zoom: 1 });
      })
      .catch((e) => {
        if (cancelled) return;
        setLoadError(typeof e === "string" ? e : String(e));
        const empty = emptyBlockGraph(scope);
        setGraph(empty);
        setViewport(empty.viewport);
      });
    return () => {
      cancelled = true;
      if (saveTimer.current != null) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
    };
  }, [scope.projectId, scope.workspaceId]);

  const merged = useMemo(() => {
    if (!graph) return null;
    return mergeAgentsIntoGraph(graph, useStore.getState().agents.values(), scope);
  }, [graph, agentIdSig, scope]);
  mergedRef.current = merged;

  const cardCount =
    (merged?.terminals.length ?? 0) + (merged?.agents.length ?? 0);

  useEffect(() => {
    const apply = () => {
      const req = getCanvasCenterReq();
      if (!req.agentId) return;
      const g = mergedRef.current;
      const el = surfaceRef.current;
      if (!g || !el) return;
      const term = g.terminals.find((n) => n.agentId === req.agentId);
      const cons = g.agents.find((n) => n.id === req.agentId);
      const node = term ?? cons;
      if (!node) return;
      if (centerSeqRef.current === req.seq) return;
      centerSeqRef.current = req.seq;
      const z = viewportRef.current.zoom || 1;
      const size = node.size ?? { w: 240, h: 88 };
      const next = {
        zoom: z,
        x: el.clientWidth / 2 - (node.position.x + size.w / 2) * z,
        y: el.clientHeight / 2 - (node.position.y + size.h / 2) * z,
      };
      setViewport(next);
      setSelectedId(req.agentId);
      setSelectedTermId(term?.id ?? null);
      const snap = graphRef.current ?? g;
      persist(snap, next);
    };
    const unsub = subscribeCanvasCenter(apply);
    apply();
    return unsub;
  }, [merged, persist]);

  const hasActive = useMemo(() => {
    if (!merged) return false;
    const ids = [
      ...merged.terminals.map((n) => n.agentId).filter((id): id is string => !!id),
      ...merged.agents.map((n) => n.id),
    ];
    const now = Date.now();
    return ids.some((id) => now - (agentActiveAt.get(id) ?? 0) < ACTIVE_WINDOW_MS);
  }, [merged, agentActiveAt]);

  useEffect(() => {
    if (!hasActive) return;
    const id = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [hasActive]);

  const openAddPicker = (clientX: number, clientY: number, world: CanvasVec) => {
    pendingPosRef.current.clear();
    setTermPicker({ x: clientX, y: clientY, world });
  };

  const onAddAt = (world?: CanvasVec) => {
    const el = surfaceRef.current?.getBoundingClientRect();
    const pos =
      world ??
      (el
        ? screenToWorld(el.left + el.width / 2, el.top + el.height / 2, el, viewportRef.current)
        : { x: 80, y: 80 });
    const r = surfaceRef.current?.getBoundingClientRect();
    openAddPicker(r ? r.left + 12 : 80, r ? r.top + 12 : 80, pos);
  };

  const createOnCanvas = (runtime: string, world: CanvasVec) => {
    const pendingId = `pending:${runtime}:${Date.now()}`;
    const stub = {
      id: pendingId,
      project: scope.projectId,
      runtime,
      title: runtime,
      cwd: "",
    } as const;
    placePendingOnCanvas(pendingId, stub.runtime, stub.title, world, "create");
    void spawnAgent(scope.projectId, runtime, { addTab: false })
      .then((realId) => promotePending(pendingId, realId, runtime))
      .catch(() => {
        hideNode(
          isShellRuntime(runtime) ? "terminal" : "console",
          isShellRuntime(runtime) ? `term_${pendingId}` : pendingId,
          pendingId
        );
      });
  };

  const placePendingOnCanvas = (
    agentId: string,
    runtime: string,
    title: string,
    world: CanvasVec,
    motion: "create" | "drop"
  ) => {
    playAppear(agentId, motion);
    const display = mergedRef.current;
    const occupied: { pos: CanvasVec; size: { w: number; h: number } }[] = [];
    for (const n of display?.terminals ?? []) {
      occupied.push({
        pos: n.position,
        size: expandedSizes[n.agentId ?? n.id] ?? CARD,
      });
    }
    for (const n of display?.agents ?? []) {
      occupied.push({
        pos: n.position,
        size: expandedSizes[n.id] ?? CARD,
      });
    }
    const dest =
      motion === "create"
        ? findFreeWorldPos(world, occupied, CARD, layout.gap)
        : world;
    const el = surfaceRef.current;
    if (motion === "create" && el) {
      const z = viewportRef.current.zoom || 1;
      springCameraTo({
        zoom: z,
        x: el.clientWidth / 2 - (dest.x + CARD.w / 2) * z,
        y: el.clientHeight / 2 - (dest.y + CARD.h / 2) * z,
      });
    }
    setGraph((prev) => {
      const base = prev ?? emptyBlockGraph(scope);
      if (isShellRuntime(runtime)) {
        const termId = `term_${agentId}`;
        const next = {
          ...base,
          terminals: [
            ...base.terminals,
            {
              id: termId,
              name: title,
              cwd: "",
              command: "",
              kind: "task" as const,
              agentId,
              position: dest,
              size: { w: CARD.w, h: CARD.h },
            },
          ],
        };
        persist(next, viewportRef.current);
        return next;
      }
      const next = {
        ...base,
        agents: [...base.agents, { id: agentId, position: dest, size: { w: CARD.w, h: CARD.h } }],
      };
      persist(next, viewportRef.current);
      return next;
    });
  };

  const promotePending = (pendingId: string, realId: string, runtime: string) => {
    pendingAppearRef.current[realId] = pendingAppearRef.current[pendingId];
    delete pendingAppearRef.current[pendingId];
    setExpandedSizes((s) => {
      if (!s[pendingId]) return s;
      const { [pendingId]: size, ...rest } = s;
      return { ...rest, [realId]: size };
    });
    setGraph((prev) => {
      if (!prev) return prev;
      const next = isShellRuntime(runtime)
        ? {
            ...prev,
            terminals: prev.terminals.map((n) =>
              n.agentId === pendingId
                ? { ...n, id: `term_${realId}`, agentId: realId }
                : n
            ),
          }
        : {
            ...prev,
            agents: prev.agents.map((n) => (n.id === pendingId ? { ...n, id: realId } : n)),
          };
      persist(next, viewportRef.current);
      return next;
    });
  };

  const playAppear = (agentId: string, kind: "create" | "drop") => {
    pendingAppearRef.current[agentId] = kind;
  };

  const springCameraTo = (target: CanvasViewport) => {
    if (camRafRef.current != null) cancelAnimationFrame(camRafRef.current);
    const worldEl = worldRef.current;
    if (
      !worldEl ||
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ) {
      viewportRef.current = target;
      setViewport(target);
      return;
    }
    const wn = (2 * Math.PI) / 0.36;
    let x = viewportRef.current.x;
    let y = viewportRef.current.y;
    let z = viewportRef.current.zoom || 1;
    let vx = 0;
    let vy = 0;
    let vz = 0;
    let last = performance.now();
    const step = (value: number, vel: number, dest: number, dt: number) => {
      const displacement = value - dest;
      const velocityTerm = vel + wn * displacement;
      const decay = Math.exp(-wn * dt);
      return {
        value: dest + (displacement + velocityTerm * dt) * decay,
        velocity: (vel - wn * velocityTerm * dt) * decay,
      };
    };
    const tick = (now: number) => {
      const dt = Math.min(1 / 30, Math.max(0, (now - last) / 1000));
      last = now;
      const sx = step(x, vx, target.x, dt);
      const sy = step(y, vy, target.y, dt);
      const sz = step(z, vz, target.zoom, dt);
      x = sx.value;
      vx = sx.velocity;
      y = sy.value;
      vy = sy.velocity;
      z = sz.value;
      vz = sz.velocity;
      worldEl.style.transform = `translate(${x}px, ${y}px) scale(${z})`;
      if (
        Math.abs(x - target.x) <= 0.4 &&
        Math.abs(y - target.y) <= 0.4 &&
        Math.abs(z - target.zoom) <= 0.002 &&
        Math.abs(vx) <= 1 &&
        Math.abs(vy) <= 1 &&
        Math.abs(vz) <= 0.02
      ) {
        camRafRef.current = null;
        viewportRef.current = target;
        setViewport(target);
        return;
      }
      camRafRef.current = requestAnimationFrame(tick);
    };
    camRafRef.current = requestAnimationFrame(tick);
  };

  const placeAgentOnCanvas = (agentId: string, world: CanvasVec, motion: "create" | "drop" = "drop") => {
    const agent = useStore.getState().agents.get(agentId);
    if (!agent) return;
    if (agent.project && agent.project !== scope.projectId) {
      notify(t("canvas.dropWrongProject"), agent.title || agentId);
      return;
    }
    const persisted = graphRef.current;
    const onGraph = !!(
      persisted?.terminals.some((n) => n.agentId === agentId || n.id === `term_${agentId}`) ||
      persisted?.agents.some((n) => n.id === agentId)
    );
    if (!onGraph) playAppear(agentId, motion);
    const display = mergedRef.current;
    let dest = world;
    if (motion === "create" && !onGraph) {
      const occupied: { pos: CanvasVec; size: { w: number; h: number } }[] = [];
      for (const n of display?.terminals ?? []) {
        if (n.agentId === agentId) continue;
        occupied.push({
          pos: n.position,
          size: expandedSizes[n.agentId ?? n.id] ?? CARD,
        });
      }
      for (const n of display?.agents ?? []) {
        if (n.id === agentId) continue;
        occupied.push({
          pos: n.position,
          size: expandedSizes[n.id] ?? CARD,
        });
      }
      dest = findFreeWorldPos(world, occupied, EXPANDED_MIN);
    }
    const el = surfaceRef.current;
    let vp = viewportRef.current;
    if (motion === "create" && !onGraph && el) {
      const z = vp.zoom || 1;
      springCameraTo({
        zoom: z,
        x: el.clientWidth / 2 - (dest.x + EXPANDED_MIN.w / 2) * z,
        y: el.clientHeight / 2 - (dest.y + EXPANDED_MIN.h / 2) * z,
      });
    }
    setGraph((prev) => {
      const base = prev ?? emptyBlockGraph(scope);
      const hidden = (base.agentsHidden ?? []).filter((id) => id !== agentId);
      if (isShellRuntime(agent.runtime)) {
        const termId = `term_${agentId}`;
        const terminals = base.terminals.some((n) => n.agentId === agentId || n.id === termId)
          ? base.terminals.map((n) =>
              n.agentId === agentId || n.id === termId ? { ...n, position: dest } : n
            )
          : [
              ...base.terminals,
              {
                id: termId,
                name: agent.title,
                cwd: agent.cwd,
                command: "",
                kind: "task" as const,
                agentId,
                position: dest,
                size: { w: 240, h: 88 },
              },
            ];
        const next = { ...base, terminals, agentsHidden: hidden };
        persist(next, viewportRef.current);
        return next;
      }
      const agentsLayout = base.agents.some((n) => n.id === agentId)
        ? base.agents.map((n) => (n.id === agentId ? { ...n, position: dest } : n))
        : [...base.agents, { id: agentId, position: dest, size: { w: 240, h: 88 } }];
      const next = { ...base, agents: agentsLayout, agentsHidden: hidden };
      persist(next, viewportRef.current);
      return next;
    });
  };

  useEffect(() => {
    if (!merged || pendingPosRef.current.size === 0) return;
    let changed = false;
    const terminals = merged.terminals.map((n) => {
      if (!n.agentId) return n;
      const pos = pendingPosRef.current.get(n.agentId);
      if (!pos) return n;
      pendingPosRef.current.delete(n.agentId);
      changed = true;
      return { ...n, position: pos };
    });
    if (!changed) return;
    const next = { ...merged, terminals };
    setGraph(next);
    persist(next, viewportRef.current);
  }, [merged, persist]);

  const hideNode = (kind: "terminal" | "console", id: string, agentId: string | null) => {
    setGraph((prev) => {
      const base = mergedRef.current ?? prev;
      if (!base) return prev;
      const next =
        kind === "terminal"
          ? {
              ...base,
              terminals: base.terminals.filter((n) => n.id !== id),
              edges: base.edges.filter((e) => e.source !== id && e.target !== id),
            }
          : {
              ...base,
              agentsHidden: [...(base.agentsHidden ?? []), id],
              agents: base.agents.filter((a) => a.id !== id),
            };
      persist(next, viewportRef.current);
      return next;
    });
    if (selectedId === agentId || selectedId === id) setSelectedId(null);
    if (selectedTermId === id) setSelectedTermId(null);
  };

  const connectTerminals = (source: string, target: string) => {
    if (source === target) return;
    void invoke<BlockGraph>("canvas_graph_connect", {
      project: scope.projectId,
      workspaceId: scope.workspaceId,
      source,
      target,
    })
      .then((g) => setGraph(g))
      .catch((e) => {
        notify(t("canvas.connectFailed"), typeof e === "string" ? e : String(e));
      });
  };

  const deleteEdge = (edgeId: string) => {
    setGraph((prev) => {
      const base = mergedRef.current ?? prev;
      if (!base) return prev;
      const next = { ...base, edges: base.edges.filter((e) => e.id !== edgeId) };
      persist(next, viewportRef.current);
      return next;
    });
    setSelectedEdgeId(null);
  };

  const fitView = () => {
    const g = mergedRef.current;
    const el = surfaceRef.current;
    if (!g || !el) return;
    const sizeOf = (agentId: string | null | undefined) =>
      (agentId ? expandedSizes[agentId] : undefined) ?? CARD;
    const nodes = [
      ...g.terminals.map((n) => {
        const size = sizeOf(n.agentId);
        return { x: n.position.x, y: n.position.y, w: size.w, h: size.h };
      }),
      ...g.agents.map((n) => {
        const size = sizeOf(n.id);
        return { x: n.position.x, y: n.position.y, w: size.w, h: size.h };
      }),
    ];
    if (nodes.length === 0) {
      const next = { x: 0, y: 0, zoom: 1 };
      springCameraTo(next);
      if (graphRef.current) persist(graphRef.current, next);
      return;
    }
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + n.w);
      maxY = Math.max(maxY, n.y + n.h);
    }
    const pad = 48;
    const viewW = el.clientWidth;
    const viewH = el.clientHeight;
    const bw = Math.max(1, maxX - minX);
    const bh = Math.max(1, maxY - minY);
    const zoom = clamp(
      Math.min((viewW - pad * 2) / bw, (viewH - pad * 2) / bh),
      MIN_ZOOM,
      MAX_ZOOM
    );
    const next = {
      zoom,
      x: (viewW - bw * zoom) / 2 - minX * zoom,
      y: (viewH - bh * zoom) / 2 - minY * zoom,
    };
    springCameraTo(next);
    if (graphRef.current) persist(graphRef.current, next);
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.closest(".xterm"))) {
        return;
      }
      if (e.key === "Escape") {
        setCtxMenu(null);
        setTermPicker(null);
        setSelectedEdgeId(null);
        return;
      }
      if (e.key === "Enter" && selectedId) {
        e.preventDefault();
        focusAgentTab(selectedId);
        return;
      }
      if ((e.key === "Delete" || e.key === "Backspace") && selectedEdgeId) {
        e.preventDefault();
        deleteEdge(selectedEdgeId);
        return;
      }
      if ((e.key === "Delete" || e.key === "Backspace") && selectedTermId) {
        e.preventDefault();
        hideNode("terminal", selectedTermId, selectedId);
        return;
      }
      if (e.ctrlKey && e.key === "0") {
        e.preventDefault();
        const next = { x: 0, y: 0, zoom: 1 };
        setViewport(next);
        if (graphRef.current) persist(graphRef.current, next);
        return;
      }
      if (!selectedTermId && !selectedId) return;
      const step = e.shiftKey ? 20 : 8;
      let dx = 0, dy = 0;
      if (e.key === "ArrowLeft") dx = -step;
      else if (e.key === "ArrowRight") dx = step;
      else if (e.key === "ArrowUp") dy = -step;
      else if (e.key === "ArrowDown") dy = step;
      else return;
      e.preventDefault();
      setGraph((prev) => {
        const base = mergedRef.current ?? prev;
        if (!base) return prev;
        const next = selectedTermId
          ? {
              ...base,
              terminals: base.terminals.map((n) =>
                n.id === selectedTermId
                  ? { ...n, position: { x: n.position.x + dx, y: n.position.y + dy } }
                  : n
              ),
            }
          : {
              ...base,
              agents: base.agents.map((n) =>
                n.id === selectedId
                  ? { ...n, position: { x: n.position.x + dx, y: n.position.y + dy } }
                  : n
              ),
            };
        persist(next, viewportRef.current);
        return next;
      });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedId, selectedTermId, selectedEdgeId, persist]);

  useEffect(() => {
    const el = surfaceRef.current;
    if (!el) return;
    const onWheelNative = (e: WheelEvent) => {
      if ((e.target as HTMLElement | null)?.closest(".canvas-card-pty")) return;
      e.preventDefault();
      e.stopPropagation();
      const dy = e.deltaY !== 0 ? e.deltaY : 0;
      if (dy === 0) return;
      const vp = viewportRef.current;
      const cx = el.clientWidth / 2;
      const cy = el.clientHeight / 2;
      const steps = e.deltaMode === 1 ? Math.abs(dy) : Math.min(6, Math.abs(dy) / 50);
      const factor = dy < 0 ? 1 + 0.06 * Math.max(1, steps) : 1 / (1 + 0.06 * Math.max(1, steps));
      const zoom = clamp(vp.zoom * factor, MIN_ZOOM, MAX_ZOOM);
      if (zoom === vp.zoom) return;
      const scale = zoom / (vp.zoom || 1);
      const next = {
        zoom,
        x: cx - (cx - vp.x) * scale,
        y: cy - (cy - vp.y) * scale,
      };
      setViewport(next);
      const snap = mergedRef.current ?? graphRef.current;
      if (snap) persist(snap, next);
    };
    el.addEventListener("wheel", onWheelNative, { passive: false, capture: true });
    return () => el.removeEventListener("wheel", onWheelNative, true);
  }, [persist]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "t")) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      const st = useStore.getState();
      const runtime = resolveCtrlTRuntime(st.ctrlTRuntime, st.runtimes);
      if (!runtime) {
        notify(t("content.cannotCreateTerminal"), t("content.noRuntimeBody"));
        return;
      }
      const el = surfaceRef.current?.getBoundingClientRect();
      const world = el
        ? screenToWorld(
            el.left + el.width / 2,
            el.top + el.height / 2,
            el,
            viewportRef.current
          )
        : { x: 80, y: 80 };
      createOnCanvas(runtime, world);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [scope.projectId, t]);

  const onSurfacePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button === 2) return;
    if (e.button !== 0 && e.button !== 1) return;
    const tEl = e.target as HTMLElement;
    if (tEl.closest(".canvas-card, .canvas-card-pty, .ctx-menu, .canvas-connect-handle")) {
      return;
    }
    e.preventDefault();
    if (camRafRef.current != null) {
      cancelAnimationFrame(camRafRef.current);
      camRafRef.current = null;
    }
    setSelectedId(null);
    setSelectedTermId(null);
    setSelectedEdgeId(null);
    setCtxMenu(null);
    setCardMenu(null);
    panRef.current = {
      pointerId: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      orig: { ...viewportRef.current },
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onSurfacePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const resize = resizeRef.current;
    if (resize && resize.pointerId === e.pointerId) {
      const z = viewportRef.current.zoom || 1;
      const next = {
        w: Math.max(EXPANDED_MIN.w, resize.orig.w + (e.clientX - resize.start.x) / z),
        h: Math.max(EXPANDED_MIN.h, resize.orig.h + (e.clientY - resize.start.y) / z),
      };
      setExpandedSizes((s) => ({ ...s, [resize.agentId]: next }));
      return;
    }
    const conn = connectRef.current;
    if (conn && conn.pointerId === e.pointerId) {
      const rect = surfaceRef.current?.getBoundingClientRect();
      if (!rect) return;
      setConnectPreview({
        x1: conn.from.x,
        y1: conn.from.y,
        x2: e.clientX - rect.left,
        y2: e.clientY - rect.top,
      });
      return;
    }
    const pan = panRef.current;
    if (pan && pan.pointerId === e.pointerId) {
      const next = {
        ...pan.orig,
        x: pan.orig.x + (e.clientX - pan.startX),
        y: pan.orig.y + (e.clientY - pan.startY),
      };
      setViewport(next);
      return;
    }
    const drag = dragRef.current;
    if (drag && drag.pointerId === e.pointerId) {
      const dist = Math.hypot(e.clientX - drag.start.x, e.clientY - drag.start.y);
      if (!drag.armed && dist < DRAG_THRESHOLD) return;
      drag.armed = true;
      didDragRef.current = true;
      setCardDragging(true);
      surfaceRef.current?.setPointerCapture(e.pointerId);
      const z = viewportRef.current.zoom || 1;
      const dx = (e.clientX - drag.start.x) / z;
      const dy = (e.clientY - drag.start.y) / z;
      const pos = { x: drag.orig.x + dx, y: drag.orig.y + dy };
      const display = mergedRef.current;
      const applyPos = (g: BlockGraph): BlockGraph => {
        if (drag.kind === "terminal") {
          const fromDisplay = display?.terminals.find((n) => n.id === drag.id);
          if (!g.terminals.some((n) => n.id === drag.id)) {
            if (!fromDisplay) return g;
            return {
              ...g,
              terminals: [...g.terminals, { ...fromDisplay, position: pos }],
            };
          }
          return {
            ...g,
            terminals: g.terminals.map((n) =>
              n.id === drag.id ? { ...n, position: pos } : n
            ),
          };
        }
        if (!g.agents.some((n) => n.id === drag.id)) {
          const fromDisplay = display?.agents.find((n) => n.id === drag.id);
          return {
            ...g,
            agents: [
              ...g.agents,
              {
                id: drag.id,
                position: pos,
                size: fromDisplay?.size ?? { ...DEFAULT_CARD_SIZE },
              },
            ],
          };
        }
        return {
          ...g,
          agents: g.agents.map((n) =>
            n.id === drag.id ? { ...n, position: pos } : n
          ),
        };
      };
      setGraph((prev) => {
        const base = prev ?? emptyBlockGraph(scope);
        return applyPos(base);
      });
    }
  };

  const endGesture = (e: React.PointerEvent<HTMLDivElement>) => {
    const conn = connectRef.current;
    if (conn && conn.pointerId === e.pointerId) {
      connectRef.current = null;
      setConnectPreview(null);
      const hit = (e.target as HTMLElement | null)?.closest(".canvas-node") as HTMLElement | null;
      const toId = hit?.dataset.termId;
      if (toId && toId !== conn.fromId) connectTerminals(conn.fromId, toId);
    }
    const panned = panRef.current && panRef.current.pointerId === e.pointerId;
    const drag = dragRef.current;
    const dragged = drag && drag.pointerId === e.pointerId && drag.armed;
    panRef.current = null;
    dragRef.current = null;
    resizeRef.current = null;
    setCardDragging(false);
    if (panned && graphRef.current) {
      persist(graphRef.current, viewportRef.current);
    } else if (dragged && graphRef.current) {
      persist(graphRef.current, viewportRef.current);
    } else if (
      drag &&
      drag.pointerId === e.pointerId &&
      !drag.armed &&
      !(e.target as HTMLElement | null)?.closest(".canvas-card-icon-btn")
    ) {
      const display = mergedRef.current;
      const agentId =
        drag.kind === "terminal"
          ? display?.terminals.find((n) => n.id === drag.id)?.agentId
          : drag.id;
      if (agentId) {
        const now = Date.now();
        const prev = lastClickRef.current;
        if (prev && prev.id === agentId && now - prev.at < 400) {
          lastClickRef.current = null;
          focusAgentTab(agentId);
        } else {
          lastClickRef.current = { id: agentId, at: now };
        }
      }
    }
    window.setTimeout(() => {
      didDragRef.current = false;
    }, 0);
  };

  const startCardDrag = (
    e: React.PointerEvent<HTMLDivElement>,
    kind: "terminal" | "console",
    id: string,
    orig: CanvasVec
  ) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement | null)?.closest(".canvas-card-icon-btn")) return;
    e.preventDefault();
    e.stopPropagation();
    didDragRef.current = false;
    dragRef.current = {
      pointerId: e.pointerId,
      kind,
      id,
      start: { x: e.clientX, y: e.clientY },
      orig: { ...orig },
      armed: false,
    };
  };

  const startConnect = (
    e: React.PointerEvent<HTMLDivElement>,
    fromId: string,
    fromWorld: CanvasVec,
    size: { w: number; h: number }
  ) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const vp = viewportRef.current;
    const from = worldToSurface(
      { x: fromWorld.x + size.w, y: fromWorld.y + size.h / 2 },
      vp
    );
    connectRef.current = { pointerId: e.pointerId, fromId, from };
    setConnectPreview({ x1: from.x, y1: from.y, x2: from.x, y2: from.y });
    surfaceRef.current?.setPointerCapture(e.pointerId);
  };

  useEffect(() => {
    if (!run) return;
    const id = window.setInterval(() => {
      void invoke<RunStatus>("canvas_run_status", { runId: run.runId })
        .then(setRun)
        .catch(() => setRun(null));
    }, 500);
    return () => window.clearInterval(id);
  }, [run?.runId]);

  useEffect(() => {
    if (!run || !merged) return;
    for (const termId of run.ready ?? []) {
      if (launchedRef.current.has(termId)) continue;
      const term = merged.terminals.find((n) => n.id === termId);
      if (!term) continue;
      launchedRef.current.add(termId);
      const finish = (code: number) => {
        void invoke<RunStatus>("canvas_run_report_exit", {
          runId: run.runId,
          terminalId: termId,
          code,
        }).then(setRun);
      };
      if (!term.command && term.kind !== "service") {
        finish(0);
        continue;
      }
      if (!term.agentId) {
        finish(1);
        continue;
      }
      void invoke<RunStatus>("canvas_run_mark_running", {
        runId: run.runId,
        terminalId: termId,
      }).then(setRun);
      const port = run.leases?.[termId];
      const data =
        port != null ? term.command.split("{PORT}").join(String(port)) : term.command;
      void invoke("agent_write", {
        id: term.agentId,
        data,
        raw: false,
      }).catch(() => finish(1));
    }
  }, [run, merged]);

  useEffect(() => {
    if (!run || !merged) return;
    for (const term of merged.terminals) {
      if (!term.agentId) continue;
      if (run.nodeStates?.[term.id] !== "running") continue;
      if (reportedRef.current.has(term.id)) continue;
      const agent = useStore.getState().agents.get(term.agentId);
      if (!agent) continue;
      if (agent.status === "done" || agent.status === "failed") {
        reportedRef.current.add(term.id);
        void invoke<RunStatus>("canvas_run_report_exit", {
          runId: run.runId,
          terminalId: term.id,
          code: agent.status === "done" ? 0 : 1,
        }).then(setRun);
      }
    }
  }, [run, merged]);

  useEffect(() => {
    if (!run || !merged) return;
    const services = merged.terminals.filter(
      (term) =>
        term.kind === "service" &&
        run.nodeStates?.[term.id] === "running" &&
        !reportedRef.current.has(term.id)
    );
    if (services.length === 0) return;
    let cancelled = false;
    const tick = () => {
      for (const term of services) {
        void invoke<boolean>("canvas_run_probe_ready", {
          runId: run.runId,
          terminalId: term.id,
        }).then((ok) => {
          if (cancelled || !ok || reportedRef.current.has(term.id)) return;
          reportedRef.current.add(term.id);
          void invoke<RunStatus>("canvas_run_report_exit", {
            runId: run.runId,
            terminalId: term.id,
            code: 0,
          }).then(setRun);
        });
      }
    };
    tick();
    const id = window.setInterval(tick, 400);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [run, merged]);

  const goToTerminal = (agentId: string) => {
    focusAgentTab(agentId);
  };

  return (
    <div className="canvas-panel">
      <CanvasToolbar onAdd={() => onAddAt()} onFit={fitView} />
      <div
        ref={surfaceRef}
        className="canvas-surface"
        onPointerDown={onSurfacePointerDown}
        onPointerMove={onSurfacePointerMove}
        onPointerUp={endGesture}
        onPointerCancel={endGesture}
        onLostPointerCapture={endGesture}
        onAuxClick={(e) => {
          if (e.button === 1) e.preventDefault();
        }}
        onDragOver={(e) => {
          if (isCanvasAgentDrag(e.dataTransfer)) {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
          }
        }}
        onDrop={(e) => {
          if (!isCanvasAgentDrag(e.dataTransfer)) return;
          e.preventDefault();
          e.stopPropagation();
          const id = getCanvasAgentDragId(e.dataTransfer);
          endCanvasAgentDrag();
          if (!id) return;
          const rect = e.currentTarget.getBoundingClientRect();
          const world = screenToWorld(e.clientX, e.clientY, rect, viewportRef.current);
          placeAgentOnCanvas(id, world);
        }}
        onContextMenu={(e) => {
          if ((e.target as HTMLElement).closest(".canvas-card, .ctx-menu")) {
            return;
          }
          e.preventDefault();
          const rect = e.currentTarget.getBoundingClientRect();
          setCtxMenu({
            x: e.clientX,
            y: e.clientY,
            world: screenToWorld(e.clientX, e.clientY, rect, viewportRef.current),
          });
        }}
      >
        {loadError && <div className="canvas-load-error">{loadError}</div>}
        {!graph && !loadError && (
          <div className="canvas-empty">{t("common.loading")}</div>
        )}
        {graph && cardCount === 0 && (
          <div className="canvas-empty">
            {t("canvas.emptyHint")}
            <div className="canvas-toolbar-hint">{t("canvas.emptyContext")}</div>
          </div>
        )}
        {merged && (
          <>
            <div className="canvas-edges" aria-hidden>
              {!cardDragging &&
                merged.edges.map((edge) => {
                const a = merged.terminals.find((n) => n.id === edge.source);
                const b = merged.terminals.find((n) => n.id === edge.target);
                if (!a || !b) return null;
                const p1 = worldToSurface(
                  { x: a.position.x + a.size.w, y: a.position.y + a.size.h / 2 },
                  viewport
                );
                const p2 = worldToSurface(
                  { x: b.position.x, y: b.position.y + b.size.h / 2 },
                  viewport
                );
                return (
                  <EdgeLine
                    key={edge.id}
                    x1={p1.x}
                    y1={p1.y}
                    x2={p2.x}
                    y2={p2.y}
                    selected={selectedEdgeId === edge.id}
                    onPointerDown={(ev) => {
                      ev.stopPropagation();
                      setSelectedEdgeId(edge.id);
                      setSelectedId(null);
                      setSelectedTermId(null);
                    }}
                  />
                );
              })}
              {connectPreview && (
                <EdgeLine
                  x1={connectPreview.x1}
                  y1={connectPreview.y1}
                  x2={connectPreview.x2}
                  y2={connectPreview.y2}
                  preview
                />
              )}
            </div>
            <div ref={worldRef} className="canvas-world">
              {merged.terminals.map((node) => {
                const agentId = node.agentId;
                if (!agentId) return null;
                const cardSize = expandedSizes[agentId] ?? CARD;
                const appear = pendingAppearRef.current[agentId];
                const livePty =
                  viewSize.w === 0 ||
                  cardOnScreen(node.position, cardSize, viewport, viewSize);
                return (
                  <div
                    key={node.id}
                    data-term-id={node.id}
                    className={`canvas-node expanded${run?.nodeStates?.[node.id] ? ` run-${run.nodeStates[node.id]}` : ""}`}
                    style={{
                      left: node.position.x,
                      top: node.position.y,
                      width: cardSize.w,
                      height: cardSize.h,
                      zIndex: 5,
                    }}
                  >
                    <CanvasAppear kind={appear}>
                    <CanvasNodeCard
                      agentId={agentId}
                      kind="terminal"
                      selected={selectedId === agentId}
                      showPty={livePty}
                      onSelect={(ev) => {
                        if (didDragRef.current) return;
                        if (ev.shiftKey && selectedTermId && selectedTermId !== node.id) {
                          connectTerminals(selectedTermId, node.id);
                          return;
                        }
                        setSelectedId(agentId);
                        setSelectedTermId(node.id);
                        setSelectedEdgeId(null);
                      }}
                      onDoubleClick={() => goToTerminal(agentId)}
                      onPointerDownDrag={(ev) =>
                        startCardDrag(ev, "terminal", node.id, node.position)
                      }
                      onConnectPointerDown={(ev) =>
                        startConnect(ev, node.id, node.position, node.size)
                      }
                      onHide={() => hideNode("terminal", node.id, agentId)}
                      onCardContextMenu={(e) => {
                        setCardMenu({
                          x: e.clientX,
                          y: e.clientY,
                          agentId,
                          kind: "terminal",
                          nodeId: node.id,
                          runtime: useStore.getState().agents.get(agentId)?.runtime ?? "",
                        });
                      }}
                    />
                    <div
                      className="canvas-pty-resize"
                      onPointerDown={(e) => {
                        e.stopPropagation();
                        resizeRef.current = {
                          pointerId: e.pointerId,
                          agentId,
                          start: { x: e.clientX, y: e.clientY },
                          orig: { ...(expandedSizes[agentId] ?? CARD) },
                        };
                        surfaceRef.current?.setPointerCapture(e.pointerId);
                      }}
                    />
                    </CanvasAppear>
                  </div>
                );
              })}
              {merged.agents.map((node) => {
                const agentId = node.id;
                const cardSize = expandedSizes[agentId] ?? CARD;
                const appear = pendingAppearRef.current[agentId];
                const livePty =
                  viewSize.w === 0 ||
                  cardOnScreen(node.position, cardSize, viewport, viewSize);
                return (
                  <div
                    key={node.id}
                    className="canvas-node expanded"
                    style={{
                      left: node.position.x,
                      top: node.position.y,
                      width: cardSize.w,
                      height: cardSize.h,
                      zIndex: 5,
                    }}
                  >
                    <CanvasAppear kind={appear}>
                    <CanvasNodeCard
                      agentId={agentId}
                      kind="console"
                      selected={selectedId === agentId}
                      showPty={livePty}
                      onSelect={() => {
                        if (didDragRef.current) return;
                        setSelectedId(agentId);
                        setSelectedTermId(null);
                        setSelectedEdgeId(null);
                      }}
                      onDoubleClick={() => goToTerminal(agentId)}
                      onPointerDownDrag={(ev) =>
                        startCardDrag(ev, "console", node.id, node.position)
                      }
                      onHide={() => hideNode("console", node.id, agentId)}
                      onCardContextMenu={(e) => {
                        setCardMenu({
                          x: e.clientX,
                          y: e.clientY,
                          agentId,
                          kind: "console",
                          nodeId: node.id,
                          runtime: useStore.getState().agents.get(agentId)?.runtime ?? "",
                        });
                      }}
                    />
                    <div
                      className="canvas-pty-resize"
                      onPointerDown={(e) => {
                        e.stopPropagation();
                        resizeRef.current = {
                          pointerId: e.pointerId,
                          agentId,
                          start: { x: e.clientX, y: e.clientY },
                          orig: { ...(expandedSizes[agentId] ?? CARD) },
                        };
                        surfaceRef.current?.setPointerCapture(e.pointerId);
                      }}
                    />
                    </CanvasAppear>
                  </div>
                );
              })}
            </div>
          </>
        )}
        {ctxMenu && (
          <div
            className="ctx-menu"
            style={{ position: "fixed", left: ctxMenu.x, top: ctxMenu.y, zIndex: 1000 }}
            onClick={(e) => e.stopPropagation()}
            onContextMenu={(e) => e.stopPropagation()}
          >
            <div
              className="ctx-item"
              onClick={() => {
                setTermPicker({ x: ctxMenu.x, y: ctxMenu.y, world: ctxMenu.world });
                setCtxMenu(null);
              }}
            >
              <Icon name="plus" size={13} /> {t("canvas.addTerminal")}
            </div>
            {selectedEdgeId && (
              <div
                className="ctx-item"
                onClick={() => {
                  deleteEdge(selectedEdgeId);
                  setCtxMenu(null);
                }}
              >
                <Icon name="x" size={13} /> {t("canvas.deleteEdge")}
              </div>
            )}
          </div>
        )}
        {cardMenu && (
          <div
            className="ctx-menu"
            style={{ position: "fixed", left: cardMenu.x, top: cardMenu.y, zIndex: 1000 }}
            onClick={(e) => e.stopPropagation()}
            onContextMenu={(e) => e.stopPropagation()}
          >
            <div
              className="ctx-item"
              onClick={() => {
                goToTerminal(cardMenu.agentId);
                setCardMenu(null);
              }}
            >
              <Icon name="square-terminal" size={13} /> {t("canvas.openTerminal")}
            </div>
            <div
              className="ctx-item"
              onClick={() => {
                hideNode(cardMenu.kind, cardMenu.nodeId, cardMenu.agentId);
                setCardMenu(null);
              }}
            >
              <Icon name="x" size={13} /> {t("canvas.hideFromCanvas")}
            </div>
            <div
              className="ctx-item"
              onClick={() => {
                const { kind, nodeId, agentId } = cardMenu;
                setCardMenu(null);
                hideNode(kind, nodeId, agentId);
                void closeAgentAction(agentId);
              }}
            >
              <Icon name="trash-2" size={13} /> {t("canvas.closeAndKill")}
            </div>
          </div>
        )}
      </div>
      {termPicker && (
        <TerminalTemplatePicker
          project={scope.projectId}
          anchor={termPicker}
          addTab={false}
          onSpawned={(id) => placeAgentOnCanvas(id, termPicker.world, "create")}
          onClose={() => {
            setTermPicker(null);
          }}
        />
      )}
    </div>
  );
}
