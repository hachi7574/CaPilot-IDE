import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
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
  setCanvasLiveCards,
  clearCanvasLiveCards,
  subscribeCanvasSelect,
  getCanvasSelectReq,
  requestCanvasSendTarget,
} from "../../state/canvas";
import { isShellRuntime } from "../../state/shellPath";
import { closeAgent as closeAgentAction, spawnAgent } from "../../state/agentActions";
import { notify } from "../../state/notify";
import { fileTab } from "../../state/openFile";
import { pathsFromDataTransfer } from "../../state/dropPaths";
import { useT } from "../../i18n";
import { Icon } from "../Icon";
import { TerminalTemplatePicker } from "../layout/TerminalTemplatePicker";
import { CanvasNodeCard } from "./CanvasNodeCard";
import { CanvasFileCard } from "./CanvasFileCard";
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
const EXPANDED_MIN = { w: 700, h: 600 };

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

/** Visual-only grid: only `position` changes. Uniform cells so rows/cols line up. */
function arrangeCardsInGrid(
  items: { id: string; size: { w: number; h: number }; pos: CanvasVec }[],
  gap: number,
  origin = { x: 80, y: 80 }
): Map<string, CanvasVec> {
  const out = new Map<string, CanvasVec>();
  if (items.length === 0) return out;
  const ordered = [...items].sort((a, b) => a.pos.y - b.pos.y || a.pos.x - b.pos.x);
  const cellW = Math.max(...ordered.map((it) => it.size.w));
  const cellH = Math.max(...ordered.map((it) => it.size.h));
  const cols = Math.max(1, Math.round(Math.sqrt(ordered.length)));
  ordered.forEach((it, i) => {
    const col = i % cols;
    const row = Math.floor(i / cols);
    out.set(it.id, {
      x: origin.x + col * (cellW + gap),
      y: origin.y + row * (cellH + gap),
    });
  });
  return out;
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
  const tabFlash = useStore((s) => s.tabFlash);

  const [graph, setGraph] = useState<BlockGraph | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [viewport, setViewport] = useState<CanvasViewport>({ x: 0, y: 0, zoom: 1 });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [marquee, setMarquee] = useState<{
    x1: number;
    y1: number;
    x2: number;
    y2: number;
  } | null>(null);
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
    kind: "terminal" | "console" | "file";
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
  const cardElsRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const flashSeenRef = useRef<Map<string, number>>(new Map());
  const flashTimerRef = useRef<Map<string, number>>(new Map());

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
    armed: boolean;
    marquee: boolean;
  } | null>(null);
  const dragRef = useRef<{
    pointerId: number;
    kind: "terminal" | "console" | "file";
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
    edge: "e" | "s" | "se";
  } | null>(null);
  const centerSeqRef = useRef(0);
  const selectSeqRef = useRef(0);

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

  useEffect(() => {
    if (!merged) return;
    const cards: { agentId: string; x: number; y: number }[] = [];
    for (const n of merged.terminals) {
      if (!n.agentId) continue;
      cards.push({ agentId: n.agentId, x: n.position.x, y: n.position.y });
    }
    for (const n of merged.agents) {
      cards.push({ agentId: n.id, x: n.position.x, y: n.position.y });
    }
    cards.sort((a, b) => a.y - b.y || a.x - b.x);
    setCanvasLiveCards(
      { projectId: scope.projectId, workspaceId: scope.workspaceId },
      cards
    );
  }, [merged, scope.projectId, scope.workspaceId]);

  useEffect(() => {
    const s = { projectId: scope.projectId, workspaceId: scope.workspaceId };
    return () => clearCanvasLiveCards(s);
  }, [scope.projectId, scope.workspaceId]);

  useEffect(() => {
    for (const [agentId, seq] of tabFlash) {
      if (flashSeenRef.current.get(agentId) === seq) continue;
      flashSeenRef.current.set(agentId, seq);
      const el = cardElsRef.current.get(agentId);
      if (!el) continue;
      const card = el.querySelector<HTMLElement>(".canvas-card");
      if (!card) continue;
      card.classList.remove("canvas-card-flash");
      void card.offsetWidth;
      card.classList.add("canvas-card-flash");
      window.clearTimeout(flashTimerRef.current.get(agentId));
      flashTimerRef.current.set(
        agentId,
        window.setTimeout(() => card.classList.remove("canvas-card-flash"), 800)
      );
    }
  }, [tabFlash]);
  useEffect(
    () => () => {
      for (const t of flashTimerRef.current.values()) window.clearTimeout(t);
    },
    []
  );

  const cardCount =
    (merged?.terminals.length ?? 0) +
    (merged?.agents.length ?? 0) +
    (merged?.files?.length ?? 0);

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
      const z = req.zoom ?? viewportRef.current.zoom ?? 1;
      // Paint size, not the persisted compact `node.size` (often 240×88).
      const size = (req.agentId ? expandedSizes[req.agentId] : undefined) ?? CARD;
      const cx = node.position.x + size.w / 2;
      const cy = node.position.y + size.h / 2;
      const next = {
        zoom: z,
        x: el.clientWidth / 2 - cx * z,
        y: el.clientHeight / 2 - cy * z,
      };
      springCameraTo(next);
      setSelectedId(req.agentId);
      setSelectedTermId(term?.id ?? null);
      const snap = graphRef.current ?? g;
      persist(snap, next);
    };
    const unsub = subscribeCanvasCenter(apply);
    apply();
    return unsub;
  }, [merged, persist, expandedSizes, CARD.w, CARD.h]);

  useEffect(() => {
    const apply = () => {
      const req = getCanvasSelectReq();
      if (selectSeqRef.current === req.seq) return;
      if (!req.agentId) {
        selectSeqRef.current = req.seq;
        setSelectedId(null);
        setSelectedTermId(null);
        setSelectedEdgeId(null);
        return;
      }
      const g = mergedRef.current;
      if (!g) return;
      const term = g.terminals.find((n) => n.agentId === req.agentId);
      const cons = g.agents.find((n) => n.id === req.agentId);
      if (!term && !cons) return;
      selectSeqRef.current = req.seq;
      setSelectedId(req.agentId);
      setSelectedTermId(term?.id ?? null);
      setSelectedEdgeId(null);
    };
    const unsub = subscribeCanvasSelect(apply);
    apply();
    return unsub;
  }, [merged]);

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
    const wn = (2 * Math.PI) / 0.28;
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
      viewportRef.current = { x, y, zoom: z };
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

  const hideNode = (kind: "terminal" | "console" | "file", id: string, agentId: string | null) => {
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
          : kind === "file"
            ? {
                ...base,
                files: (base.files ?? []).filter((n) => n.id !== id),
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
    if (agentId || id) {
      setSelectedIds((prev) => {
        const next = new Set(prev);
        if (agentId) next.delete(agentId);
        next.delete(id);
        return next;
      });
    }
  };

  const hideSelectedCards = () => {
    const ids = selectedIds.size > 0 ? selectedIds : selectedId ? new Set([selectedId]) : new Set<string>();
    if (ids.size === 0) return;
    setGraph((prev) => {
      const base = mergedRef.current ?? prev;
      if (!base) return prev;
      const dropTerms = new Set(
        base.terminals.filter((n) => n.agentId && ids.has(n.agentId)).map((n) => n.id)
      );
      const dropAgents = new Set(base.agents.filter((n) => ids.has(n.id)).map((n) => n.id));
      const dropFiles = new Set((base.files ?? []).filter((n) => ids.has(n.id)).map((n) => n.id));
      const next = {
        ...base,
        terminals: base.terminals.filter((n) => !dropTerms.has(n.id)),
        agents: base.agents.filter((n) => !dropAgents.has(n.id)),
        agentsHidden: [...(base.agentsHidden ?? []), ...dropAgents],
        files: (base.files ?? []).filter((n) => !dropFiles.has(n.id)),
        edges: base.edges.filter((e) => !dropTerms.has(e.source) && !dropTerms.has(e.target)),
      };
      persist(next, viewportRef.current);
      return next;
    });
    setSelectedIds(new Set());
    setSelectedId(null);
    setSelectedTermId(null);
  };

  const closeSelectedCards = () => {
    const ids = selectedIds.size > 0 ? selectedIds : selectedId ? new Set([selectedId]) : new Set<string>();
    if (ids.size === 0) return;
    const g = mergedRef.current;
    const fileIds = new Set((g?.files ?? []).map((n) => n.id));
    hideSelectedCards();
    for (const id of ids) {
      if (!fileIds.has(id)) void closeAgentAction(id);
    }
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

  const fitView = (g?: BlockGraph | null) => {
    const graph = g && Array.isArray(g.terminals) ? g : mergedRef.current;
    const el = surfaceRef.current;
    if (!graph || !el) return;
    const sizeOf = (agentId: string | null | undefined) =>
      (agentId ? expandedSizes[agentId] : undefined) ?? CARD;
    const nodes = [
      ...graph.terminals.map((n) => {
        const size = sizeOf(n.agentId);
        return { x: n.position.x, y: n.position.y, w: size.w, h: size.h };
      }),
      ...graph.agents.map((n) => {
        const size = sizeOf(n.id);
        return { x: n.position.x, y: n.position.y, w: size.w, h: size.h };
      }),
      ...(graph.files ?? []).map((n) => {
        const size = n.size ?? CARD;
        return { x: n.position.x, y: n.position.y, w: size.w, h: size.h };
      }),
    ];
    if (nodes.length === 0) {
      const next = { x: 0, y: 0, zoom: 1 };
      springCameraTo(next);
      persist(graph, next);
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
    persist(graph, next);
  };

  const arrangeGrid = () => {
    const g = mergedRef.current;
    if (!g) return;
    const sizeOf = (agentId: string | null | undefined) =>
      (agentId ? expandedSizes[agentId] : undefined) ?? CARD;
    const items: { id: string; size: { w: number; h: number }; pos: CanvasVec }[] = [
      ...g.terminals.map((n) => ({
        id: `t:${n.id}`,
        size: sizeOf(n.agentId),
        pos: n.position,
      })),
      ...g.agents.map((n) => ({
        id: `a:${n.id}`,
        size: sizeOf(n.id),
        pos: n.position,
      })),
      ...(g.files ?? []).map((n) => ({
        id: `f:${n.id}`,
        size: n.size ?? CARD,
        pos: n.position,
      })),
    ];
    if (items.length === 0) return;
    const placed = arrangeCardsInGrid(items, layout.gap);
    const next: BlockGraph = {
      ...g,
      terminals: g.terminals.map((n) => {
        const pos = placed.get(`t:${n.id}`);
        return pos ? { ...n, position: pos } : n;
      }),
      agents: g.agents.map((n) => {
        const pos = placed.get(`a:${n.id}`);
        return pos ? { ...n, position: pos } : n;
      }),
      files: (g.files ?? []).map((n) => {
        const pos = placed.get(`f:${n.id}`);
        return pos ? { ...n, position: pos } : n;
      }),
    };
    persist(next, viewportRef.current);
    setGraph(next);
    fitView(next);
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
        setSelectedIds(new Set());
        setMarquee(null);
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
      if ((e.key === "Delete" || e.key === "Backspace") && (selectedIds.size > 0 || selectedTermId || selectedId)) {
        e.preventDefault();
        hideSelectedCards();
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
  }, [selectedId, selectedTermId, selectedEdgeId, selectedIds, persist]);

  useEffect(() => {
    const el = surfaceRef.current;
    if (!el) return;
    const onWheelNative = (e: WheelEvent) => {
      if ((e.target as HTMLElement | null)?.closest(".canvas-card-pty, .canvas-card-body")) return;
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
    if (
      tEl.closest(
        ".canvas-card, .canvas-card-pty, .canvas-card-body, .canvas-pty-resize, .canvas-card-grip, .ctx-menu, .canvas-connect-handle"
      )
    ) {
      return;
    }
    e.preventDefault();
    if (camRafRef.current != null) {
      cancelAnimationFrame(camRafRef.current);
      camRafRef.current = null;
    }
    setSelectedEdgeId(null);
    setCtxMenu(null);
    setCardMenu(null);
    if (e.button === 1) {
      setSelectedId(null);
      setSelectedTermId(null);
      setSelectedIds(new Set());
      panRef.current = {
        pointerId: e.pointerId,
        startX: e.clientX,
        startY: e.clientY,
        orig: { ...viewportRef.current },
        armed: true,
        marquee: false,
      };
    } else {
      panRef.current = {
        pointerId: e.pointerId,
        startX: e.clientX,
        startY: e.clientY,
        orig: { ...viewportRef.current },
        armed: false,
        marquee: false,
      };
    }
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onSurfacePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const resize = resizeRef.current;
    if (resize && resize.pointerId === e.pointerId) {
      const z = viewportRef.current.zoom || 1;
      const dx = resize.edge === "s" ? 0 : (e.clientX - resize.start.x) / z;
      const dy = resize.edge === "e" ? 0 : (e.clientY - resize.start.y) / z;
      const next = {
        w: Math.max(EXPANDED_MIN.w, resize.orig.w + dx),
        h: Math.max(EXPANDED_MIN.h, resize.orig.h + dy),
      };
      const isFile = (mergedRef.current?.files ?? []).some((n) => n.id === resize.agentId);
      if (isFile) {
        setGraph((prev) => {
          const base = prev ?? emptyBlockGraph(scope);
          return {
            ...base,
            files: (base.files ?? []).map((n) =>
              n.id === resize.agentId ? { ...n, size: next } : n
            ),
          };
        });
      } else {
        setExpandedSizes((s) => ({ ...s, [resize.agentId]: next }));
      }
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
      const dist = Math.hypot(e.clientX - pan.startX, e.clientY - pan.startY);
      if (!pan.armed && dist >= DRAG_THRESHOLD) {
        pan.armed = true;
        pan.marquee = true;
        const rect = surfaceRef.current?.getBoundingClientRect();
        if (rect) {
          setMarquee({
            x1: pan.startX - rect.left,
            y1: pan.startY - rect.top,
            x2: e.clientX - rect.left,
            y2: e.clientY - rect.top,
          });
        }
      }
      if (pan.marquee) {
        const rect = surfaceRef.current?.getBoundingClientRect();
        if (rect) {
          setMarquee({
            x1: pan.startX - rect.left,
            y1: pan.startY - rect.top,
            x2: e.clientX - rect.left,
            y2: e.clientY - rect.top,
          });
        }
        return;
      }
      if (!pan.armed) return;
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
        if (drag.kind === "file") {
          const files = g.files ?? [];
          return {
            ...g,
            files: files.some((n) => n.id === drag.id)
              ? files.map((n) => (n.id === drag.id ? { ...n, position: pos } : n))
              : files,
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
    const pan = panRef.current;
    const panned = pan && pan.pointerId === e.pointerId && pan.armed && !pan.marquee;
    const marqueeing = pan && pan.pointerId === e.pointerId && pan.marquee;
    const clickedEmpty = pan && pan.pointerId === e.pointerId && !pan.armed;
    const drag = dragRef.current;
    const dragged = drag && drag.pointerId === e.pointerId && drag.armed;
    if (marqueeing) {
      const rect = surfaceRef.current?.getBoundingClientRect();
      const g = mergedRef.current;
      if (rect && g) {
        const x1 = Math.min(pan.startX, e.clientX) - rect.left;
        const y1 = Math.min(pan.startY, e.clientY) - rect.top;
        const x2 = Math.max(pan.startX, e.clientX) - rect.left;
        const y2 = Math.max(pan.startY, e.clientY) - rect.top;
        const vp = viewportRef.current;
        const hits = new Set<string>();
        let firstTerm: string | null = null;
        let firstId: string | null = null;
        const hit = (agentId: string, pos: CanvasVec, size: { w: number; h: number }, termId?: string) => {
          const sx = vp.x + pos.x * (vp.zoom || 1);
          const sy = vp.y + pos.y * (vp.zoom || 1);
          const sw = size.w * (vp.zoom || 1);
          const sh = size.h * (vp.zoom || 1);
          if (sx + sw < x1 || sy + sh < y1 || sx > x2 || sy > y2) return;
          hits.add(agentId);
          if (!firstId) firstId = agentId;
          if (termId && !firstTerm) firstTerm = termId;
        };
        for (const n of g.terminals) {
          if (!n.agentId) continue;
          hit(n.agentId, n.position, expandedSizes[n.agentId] ?? CARD, n.id);
        }
        for (const n of g.agents) {
          hit(n.id, n.position, expandedSizes[n.id] ?? CARD);
        }
        for (const n of g.files ?? []) {
          hit(n.id, n.position, n.size ?? CARD);
        }
        setSelectedIds(hits);
        setSelectedId(firstId);
        setSelectedTermId(firstTerm);
        if (hits.size === 1 && firstId && getCanvasLayoutPrefs().selectSyncsSendTarget) {
          requestCanvasSendTarget(firstId);
        }
      }
      setMarquee(null);
    } else if (clickedEmpty) {
      setSelectedIds(new Set());
      setSelectedId(null);
      setSelectedTermId(null);
      setMarquee(null);
    }
    panRef.current = null;
    const resizedFile = resizeRef.current && resizeRef.current.pointerId === e.pointerId;
    dragRef.current = null;
    resizeRef.current = null;
    setCardDragging(false);
    if (panned && graphRef.current) {
      persist(graphRef.current, viewportRef.current);
    } else if ((dragged || resizedFile) && graphRef.current) {
      persist(graphRef.current, viewportRef.current);
    } else if (
      drag &&
      drag.pointerId === e.pointerId &&
      !drag.armed &&
      drag.kind !== "file" &&
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

  const startCardResize = (
    e: React.PointerEvent<HTMLDivElement>,
    agentId: string,
    orig: { w: number; h: number },
    edge: "e" | "s" | "se" = "se"
  ) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    resizeRef.current = {
      pointerId: e.pointerId,
      agentId,
      start: { x: e.clientX, y: e.clientY },
      orig: { ...orig },
      edge,
    };
    surfaceRef.current?.setPointerCapture(e.pointerId);
  };

  const startCardDrag = (
    e: React.PointerEvent<HTMLDivElement>,
    kind: "terminal" | "console" | "file",
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

  const importDroppedPaths = useCallback(
    async (paths: string[]) => {
      const unique = [...new Set(paths.map((p) => p.trim()).filter(Boolean))];
      if (unique.length === 0) return;
      const s = useStore.getState();
      const root = s.projectRoots[scope.projectId];
      const destDir = root || unique[0].replace(/[\\/][^\\/]+$/, "") || ".";
      const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
      const rootNorm = root ? norm(root) : "";
      for (const src of unique) {
        try {
          const inProject =
            !!rootNorm &&
            (norm(src) === rootNorm || norm(src).startsWith(`${rootNorm}/`));
          const dest = inProject
            ? src
            : await invoke<string>("fs_paste", {
                src,
                destDir,
                isMove: false,
              });
          const name = dest.replace(/\\/g, "/").split("/").pop() || dest;
          const el = surfaceRef.current;
          const vp = viewportRef.current;
          const world = el
            ? screenToWorld(
                el.getBoundingClientRect().left + el.clientWidth / 2,
                el.getBoundingClientRect().top + el.clientHeight / 2,
                el.getBoundingClientRect(),
                vp
              )
            : { x: 80, y: 80 };
          setGraph((prev) => {
            const base = prev ?? emptyBlockGraph(scope);
            const files = base.files ?? [];
            if (files.some((f) => f.path === dest)) return base;
            const occupied = [
              ...base.terminals.map((n) => ({
                pos: n.position,
                size: expandedSizes[n.agentId ?? n.id] ?? CARD,
              })),
              ...base.agents.map((n) => ({
                pos: n.position,
                size: expandedSizes[n.id] ?? CARD,
              })),
              ...files.map((n) => ({ pos: n.position, size: n.size ?? CARD })),
            ];
            const destPos = findFreeWorldPos(world, occupied, CARD, layout.gap);
            const next = {
              ...base,
              files: [
                ...files,
                {
                  id: `file_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`,
                  path: dest,
                  name,
                  position: destPos,
                  size: { w: CARD.w, h: CARD.h },
                },
              ],
            };
            persist(next, viewportRef.current);
            return next;
          });
        } catch (err) {
          notify(t("canvas.dropFileFailed"), typeof err === "string" ? err : String(err));
        }
      }
    },
    [scope.projectId, t, CARD.w, CARD.h, layout.gap, persist, scope]
  );

  useEffect(() => {
    const onPathDrop = (ev: Event) => {
      const detail = (ev as CustomEvent).detail as
        | { paths?: string[]; kind?: string }
        | undefined;
      if (!detail || detail.kind !== "canvas") return;
      if (Array.isArray(detail.paths) && detail.paths.length) {
        void importDroppedPaths(detail.paths);
      }
    };
    window.addEventListener("capilot:path-drop", onPathDrop as EventListener);
    return () =>
      window.removeEventListener("capilot:path-drop", onPathDrop as EventListener);
  }, [importDroppedPaths]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    const el = surfaceRef.current;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type !== "drop" || !p.paths?.length) return;
        const surface = el ?? surfaceRef.current;
        if (!surface) return;
        const r = surface.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        const x = p.position.x / dpr;
        const y = p.position.y / dpr;
        if (x < r.left || x > r.right || y < r.top || y > r.bottom) return;
        const overFiles = document.elementFromPoint(x, y)?.closest("[data-path-drop=\"files\"]");
        if (overFiles) return;
        void importDroppedPaths(p.paths);
      })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [importDroppedPaths]);

  return (
    <div className="canvas-panel">
      <CanvasToolbar
        onAdd={() => onAddAt()}
        onFit={() => fitView()}
        onArrange={arrangeGrid}
        canArrange={cardCount > 0}
      />
      <div
        ref={surfaceRef}
        data-path-drop="canvas"
        className={`canvas-surface${marquee ? " marquee" : ""}`}
        onPointerDown={onSurfacePointerDown}
        onPointerMove={onSurfacePointerMove}
        onPointerUp={endGesture}
        onPointerCancel={endGesture}
        onLostPointerCapture={endGesture}
        onAuxClick={(e) => {
          if (e.button === 1) e.preventDefault();
        }}
        onDragOver={(e) => {
          if (isCanvasAgentDrag(e.dataTransfer) || pathsFromDataTransfer(e.dataTransfer).length) {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
          }
        }}
        onDrop={(e) => {
          const paths = pathsFromDataTransfer(e.dataTransfer);
          if (paths.length) {
            e.preventDefault();
            e.stopPropagation();
            void importDroppedPaths(paths);
            return;
          }
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
                  selectedId === agentId ||
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
                    ref={(el) => {
                      if (el) cardElsRef.current.set(agentId, el);
                      else cardElsRef.current.delete(agentId);
                    }}
                  >
                    <CanvasAppear kind={appear}>
                    <CanvasNodeCard
                      agentId={agentId}
                      kind="terminal"
                      selected={selectedIds.size <= 1 && selectedId === agentId}
                      marked={selectedIds.has(agentId)}
                      showPty={livePty}
                      onSelect={(ev) => {
                        if (didDragRef.current) return;
                        if (ev.shiftKey && selectedTermId && selectedTermId !== node.id) {
                          connectTerminals(selectedTermId, node.id);
                          return;
                        }
                        setSelectedId(agentId);
                        setSelectedTermId(node.id);
                        setSelectedIds(new Set([agentId]));
                        setSelectedEdgeId(null);
                        if (getCanvasLayoutPrefs().selectSyncsSendTarget) {
                          requestCanvasSendTarget(agentId);
                        }
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
                        const multi = selectedIds.size > 1 && selectedIds.has(agentId);
                        if (!multi) {
                          setSelectedId(agentId);
                          setSelectedTermId(node.id);
                          setSelectedIds(new Set([agentId]));
                        }
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
                      onPointerDown={(e) =>
                        startCardResize(e, agentId, expandedSizes[agentId] ?? CARD, "se")
                      }
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
                  selectedId === agentId ||
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
                    ref={(el) => {
                      if (el) cardElsRef.current.set(agentId, el);
                      else cardElsRef.current.delete(agentId);
                    }}
                  >
                    <CanvasAppear kind={appear}>
                    <CanvasNodeCard
                      agentId={agentId}
                      kind="console"
                      selected={selectedIds.size <= 1 && selectedId === agentId}
                      marked={selectedIds.has(agentId)}
                      showPty={livePty}
                      onSelect={() => {
                        if (didDragRef.current) return;
                        setSelectedId(agentId);
                        setSelectedTermId(null);
                        setSelectedIds(new Set([agentId]));
                        setSelectedEdgeId(null);
                        if (getCanvasLayoutPrefs().selectSyncsSendTarget) {
                          requestCanvasSendTarget(agentId);
                        }
                      }}
                      onDoubleClick={() => goToTerminal(agentId)}
                      onPointerDownDrag={(ev) =>
                        startCardDrag(ev, "console", node.id, node.position)
                      }
                      onHide={() => hideNode("console", node.id, agentId)}
                      onCardContextMenu={(e) => {
                        const multi = selectedIds.size > 1 && selectedIds.has(agentId);
                        if (!multi) {
                          setSelectedId(agentId);
                          setSelectedTermId(null);
                          setSelectedIds(new Set([agentId]));
                        }
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
                      onPointerDown={(e) =>
                        startCardResize(e, agentId, expandedSizes[agentId] ?? CARD, "se")
                      }
                    />
                    </CanvasAppear>
                  </div>
                );
              })}
              {(merged.files ?? []).map((node) => {
                const cardSize = node.size ?? CARD;
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
                    ref={(el) => {
                      if (el) cardElsRef.current.set(node.id, el);
                      else cardElsRef.current.delete(node.id);
                    }}
                  >
                    <CanvasFileCard
                      path={node.path}
                      name={node.name}
                      selected={selectedIds.size <= 1 && selectedId === node.id}
                      marked={selectedIds.has(node.id)}
                      onSelect={() => {
                        if (didDragRef.current) return;
                        setSelectedId(node.id);
                        setSelectedTermId(null);
                        setSelectedIds(new Set([node.id]));
                        setSelectedEdgeId(null);
                      }}
                      onDoubleClick={() => {
                        useStore.getState().addTab(fileTab(node.path, node.name));
                      }}
                      onPointerDownDrag={(ev) =>
                        startCardDrag(ev, "file", node.id, node.position)
                      }
                      onResizePointerDown={(ev, edge) => {
                        startCardResize(ev, node.id, cardSize, edge);
                      }}
                      onCardContextMenu={(e) => {
                        const multi = selectedIds.size > 1 && selectedIds.has(node.id);
                        if (!multi) {
                          setSelectedId(node.id);
                          setSelectedTermId(null);
                          setSelectedIds(new Set([node.id]));
                        }
                        setCardMenu({
                          x: e.clientX,
                          y: e.clientY,
                          agentId: node.id,
                          kind: "file",
                          nodeId: node.id,
                          runtime: "",
                        });
                      }}
                    />
                  </div>
                );
              })}
            </div>
            {marquee && (
              <div
                className="canvas-marquee"
                style={{
                  left: Math.min(marquee.x1, marquee.x2),
                  top: Math.min(marquee.y1, marquee.y2),
                  width: Math.abs(marquee.x2 - marquee.x1),
                  height: Math.abs(marquee.y2 - marquee.y1),
                }}
              />
            )}
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
            {selectedIds.size > 1 && selectedIds.has(cardMenu.agentId) ? (
              <>
                <div
                  className="ctx-item"
                  onClick={() => {
                    hideSelectedCards();
                    setCardMenu(null);
                  }}
                >
                  <Icon name="x" size={13} /> {t("canvas.hideSelected")}
                </div>
                <div
                  className="ctx-item"
                  onClick={() => {
                    closeSelectedCards();
                    setCardMenu(null);
                  }}
                >
                  <Icon name="trash-2" size={13} /> {t("canvas.closeSelected")}
                </div>
              </>
            ) : cardMenu.kind === "file" ? (
              <>
                <div
                  className="ctx-item"
                  onClick={() => {
                    const g = mergedRef.current;
                    const file = g?.files?.find((n) => n.id === cardMenu.nodeId);
                    if (file) useStore.getState().addTab(fileTab(file.path, file.name));
                    setCardMenu(null);
                  }}
                >
                  <Icon name="file-text" size={13} /> {t("canvas.openFile")}
                </div>
                <div
                  className="ctx-item"
                  onClick={() => {
                    hideNode("file", cardMenu.nodeId, cardMenu.agentId);
                    setCardMenu(null);
                  }}
                >
                  <Icon name="x" size={13} /> {t("canvas.hideFromCanvas")}
                </div>
              </>
            ) : (
              <>
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
              </>
            )}
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
