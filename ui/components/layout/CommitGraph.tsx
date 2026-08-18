import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useT, getLocale } from "../../i18n";
import { layout, type GitLogEntry, type PlacedCommit } from "./commitGraphLayout";

export type { GitLogEntry } from "./commitGraphLayout";

/** Lane width / row height in px. */
const COL_W = 16;
const ROW_H = 26;
/** Commit dot radius. */
const DOT_R = 4;
/** The mono family the pill renders with (CSS `.gg-pill` → `var(--mono)`, which
 *  the right sidebar rebinds to `--panel-ui-font`). Resolved once from the root
 *  so the canvas measurement matches the SVG text; falls back to the generic
 *  stack when custom properties can't be read (non-browser / measurement error). */
function monoFontStack(): string {
  try {
    if (typeof document !== "undefined") {
      const el = document.documentElement;
      const family =
        getComputedStyle(el).getPropertyValue("--panel-ui-font").trim() ||
        getComputedStyle(el).getPropertyValue("--mono").trim();
      if (family && !family.includes("var(")) return family;
    }
  } catch { /* ignore → fallback */ }
  return "ui-monospace, monospace";
}

/** Approx advance width of one mono char at the pill's 9px font, measured once
 *  so the pill rect hugs its text (the old 11px estimate left empty space and
 *  pushed the commit subject right). Fallback is the old conservative value. */
const PILL_CHAR_W = (() => {
  try {
    if (typeof document !== "undefined") {
      const ctx = document.createElement("canvas").getContext("2d");
      if (ctx) {
        ctx.font = `9px ${monoFontStack()}`;
        return ctx.measureText("MMMMMMMMMM").width / 10;
      }
    }
  } catch { /* non-browser / measurement failure → keep fallback */ }
  return 6.6;
})();
/** Dot palette per lane column (dark-bg friendly). Values live in CSS :root. */
const LANE_COLORS = ["var(--lane-0)", "var(--lane-1)", "var(--lane-2)", "var(--lane-3)", "var(--lane-4)", "var(--lane-5)", "var(--lane-6)"];

/** Unix-seconds timestamp → locale-aware date/time for the hover tooltip. */
function fmtTsFull(sec: number): string {
  if (!sec) return "—";
  const locale = getLocale() === "zh" ? "zh-CN" : undefined;
  return new Date(sec * 1000).toLocaleString(locale, {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Trim text to `max` chars with an ellipsis; 0/negative max yields "". */
function clip(text: string, max: number): string {
  if (max <= 0) return "";
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/** A ref shown as a small pill: tags (from the backend `tag: x` prefix) get a
 *  distinct color so they read differently from branch names. */
function pillFor(ref: string): { text: string; isTag: boolean } {
  if (ref.startsWith("tag: ")) return { text: ref.slice(5), isTag: true };
  return { text: ref, isTag: false };
}

interface CommitGraphProps {
  log: GitLogEntry[];
  /** Right-click on a commit row → the Git panel opens its commit context menu. */
  onCommitContextMenu?: (e: React.MouseEvent, commit: GitLogEntry) => void;
  /** True while the commit context menu is open: suppress the hover tooltip so
   *  the two overlays never show at the same time. */
  menuOpen?: boolean;
  /** Name of the checked-out branch. Its ancestry becomes the graph's main
   *  line; commits outside it (a divergent branch, or one merely ahead of the
   *  current branch) keep their own lane so the branch structure stays visible
   *  even when the history is linear. Omitted/unknown → single-lane layout. */
  currentBranch?: string;
}

/**
 * Self-drawn SVG commit tree (no external graph library).
 *
 * Geometry comes from the pure `layout()` in commitGraphLayout.ts: rows follow
 * the git-log order (newest on top) and columns are branch lanes from a lane
 * walk. Rendering is a single `<svg>`:
 *   - one colored vertical line per lane column (through all its commits,
 *     color = the lane's `--lane-N`),
 *   - stepped elbows for cross-column parent edges (merges), drawn in the
 *     child's lane color so a branch's thread reads continuously into the
 *     merge point,
 *   - a colored dot per commit (color = lane),
 *   - small ref pills between the tree and the message,
 *   - a one-line message clipped to the sidebar width via a ResizeObserver.
 *   Interactions (hover tooltip, right-click menu) live on a layer of
 *   transparent HTML `<div>`s stacked over the SVG, not on the SVG itself:
 *   WebKitGTK paints a solid black box when an SVG element is the HTML5 drag
 *   source, and the native SVG `<title>` renders a huge black GTK tooltip.
 *   Full hash/subject/author/date show in the custom hover tooltip.
 */
export function CommitGraph({ log, onCommitContextMenu, menuOpen = false, currentBranch }: CommitGraphProps) {
  const t = useT();
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerW, setContainerW] = useState(320);
  // Custom hover tooltip (the native SVG <title> renders a huge black GTK box
  // on WebKitGTK). Positioned below the hovered row; cleared on mouseleave.
  const [hover, setHover] = useState<{ left: number; top: number; c: GitLogEntry } | null>(null);
  const placed = useMemo(() => layout(log, currentBranch), [log, currentBranch]);
  const { commits, numCols, maxRow } = placed;

  // Track the sidebar width so subject clipping follows the panel, not the
  // content. Without this a wide graph would force horizontal scroll just to
  // read one message.
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => setContainerW(el.clientWidth);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // The context menu and the hover tooltip are mutually exclusive: as soon as
  // the menu opens (from a right-click that can leave the cursor on the row),
  // drop any tooltip that is still up and keep it suppressed until it closes.
  useEffect(() => {
    if (menuOpen) setHover(null);
  }, [menuOpen]);

  if (commits.length === 0) return null;

  const treeW = numCols * COL_W;
  const xOf = (col: number) => col * COL_W + COL_W / 2;
  const yOf = (row: number) => row * ROW_H + ROW_H / 2;

  // Hovered commit (drives the full-row background highlight AND the tooltip's
  // lane-colored hash). The highlight is drawn inside the SVG so it paints like
  // the file system (solid --bg3) while the dot / pill / subject still render on
  // top of it.
  const hovered = hover
    ? commits.find((p) => p.c.hash === hover.c.hash)
    : undefined;
  const hoveredRow = hovered ? hovered.row : -1;
  const hoverColor = hovered ? LANE_COLORS[hovered.col % LANE_COLORS.length] : undefined;

  // Ref pills are placed per-row right before the message (no reserved column,
  // so rows without refs don't lose width to a wide branch name elsewhere).
  const pills = new Map<string, { text: string; isTag: boolean; w: number }>();
  let maxMsgX = treeW + 8;
  for (const p of commits) {
    const first = p.c.refs[0];
    if (!first) continue;
    const pt = pillFor(first);
    const text = clip(pt.text, 12);
    const w = text.length * PILL_CHAR_W + 14;
    const msgX = treeW + 12 + w;
    if (msgX > maxMsgX) maxMsgX = msgX;
    pills.set(p.c.hash, { text, isTag: pt.isTag, w });
  }
  // The tree is as wide as the container (or wider for many lanes); commit
  // subjects are clipped per-row by the HTML overlay, so the SVG only needs to
  // reach the right edge of the widest ref pill — no fixed subject budget.
  const svgW = Math.max(containerW, maxMsgX);
  // Tight to the last row: dangling-stub lines end inside the bottom row's cell,
  // so no extra blank below the oldest commit.
  const svgH = (maxRow + 1) * ROW_H;

  // Lane column lines: one straight vertical per column, top commit → bottom
  // commit (plus a small stub when the oldest commit's parent is truncated out
  // of the log).
  const byCol: PlacedCommit[][] = [];
  for (const p of commits) {
    if (!byCol[p.col]) byCol[p.col] = [];
    byCol[p.col].push(p);
  }
  const colLines: { x: number; y1: number; y2: number; color: string }[] = [];
  for (let col = 0; col < numCols; col++) {
    const list = byCol[col] ?? [];
    if (list.length === 0) continue;
    const first = list[0];
    const last = list[list.length - 1];
    const lastParent = last.c.parents[0];
    const stub = lastParent && !commits.some((q) => q.c.hash === lastParent) ? 8 : 0;
    colLines.push({
      x: xOf(col),
      y1: yOf(first.row),
      y2: yOf(last.row) + stub,
      color: LANE_COLORS[col % LANE_COLORS.length],
    });
  }

  // Cross-column parent edges (merges): stepped elbows instead of straight
  // diagonals. A long diagonal would slice across the lane lines / rows below
  // and crowd the text as it descends. Each edge is a 2-segment polyline —
  // straight down the child's own lane to the parent's row, then a short
  // horizontal into the parent dot. Both segments stay inside the lane area
  // (x < treeW), so they never cross the subject text or ref pills; the drop
  // lives in the child's lane, which has no dots below its tip. Each elbow is
  // stroked with the CHILD's lane color, so the branch's thread flows in its
  // own color all the way into the merge point instead of switching color
  // mid-line.
  const elbows: { x1: number; y1: number; x2: number; y2: number; color: string }[] = [];
  const pos = new Map(commits.map((p) => [p.c.hash, { x: xOf(p.col), y: yOf(p.row) }]));
  for (const p of commits) {
    const x1 = xOf(p.col);
    const y1 = yOf(p.row);
    for (const par of p.c.parents) {
      const pp = pos.get(par);
      if (pp && pp.x !== x1) {
        elbows.push({ x1, y1, x2: pp.x, y2: pp.y, color: LANE_COLORS[p.col % LANE_COLORS.length] });
      }
    }
  }

  return (
    <div className="gg-tree" ref={containerRef}>
      <svg width={svgW} height={svgH} role="img" aria-label={t("git.commitTreeAria")}>
        {colLines.map((l, i) => (
          <line key={`cl${i}`} x1={l.x} y1={l.y1} x2={l.x} y2={l.y2}
            stroke={l.color} strokeOpacity={0.45} strokeWidth={1.5} strokeLinecap="round" />
        ))}
        {elbows.map((d, i) => (
          <polyline key={`eg${i}`} points={`${d.x1},${d.y1} ${d.x1},${d.y2} ${d.x2},${d.y2}`}
            fill="none" stroke={d.color} strokeOpacity={0.45} strokeWidth={1.5}
            strokeLinecap="round" strokeLinejoin="round" />
        ))}
        {/* Hovered-row background: solid --bg3 like the file tree rows, drawn
            above the lane lines but below the dot/pill/subject glyphs so the
            commit stays fully readable while the row reads as highlighted. */}
        {hoveredRow >= 0 && (
          <rect x={0} y={hoveredRow * ROW_H} width={svgW} height={ROW_H} fill="var(--bg3)" />
        )}
        {commits.map((p) => {
          const x = xOf(p.col);
          const y = yOf(p.row);
          const pill = pills.get(p.c.hash);
          return (
            <g key={p.c.hash}>
              {pill && (
                <g transform={`translate(${treeW + 4}, ${y})`}>
                  <rect x={0} y={-7} width={pill.w} height={14} rx={4}
                    fill="var(--bg3)" stroke={pill.isTag ? "rgb(var(--warn-rgb) / 0.45)" : "var(--rule2)"} />
                  <text x={7} y={1} alignmentBaseline="central"
                    fill={pill.isTag ? "var(--warn)" : "var(--ink2)"}
                    className="gg-pill">{pill.text}</text>
                </g>
              )}
              <circle cx={x} cy={y} r={DOT_R} fill={LANE_COLORS[p.col % LANE_COLORS.length]} />
            </g>
          );
        })}
      </svg>
      {/* Interactive layer: transparent HTML rows stacked over the SVG. The
          SVG stays purely visual because WebKitGTK paints a solid black box
          when an SVG element is the HTML5 drag source. These <div>s carry the
          hover tooltip and right-click menu instead. */}
      {commits.map((p) => {
        // Commit subject is an HTML overlay, not SVG: CSS `text-overflow:
        // ellipsis` clips it at the row's own right edge, so every row uses its
        // full available width (limited only by the panel window) instead of a
        // shared character budget, and it can never overflow the line.
        const pill = pills.get(p.c.hash);
        const msgX = pill ? treeW + 12 + pill.w : treeW + 8;
        return (
          <div
            key={`row-${p.c.hash}`}
            className="gg-row"
            style={{ top: p.row * ROW_H, width: svgW, height: ROW_H }}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setHover(null);
              onCommitContextMenu?.(e, p.c);
            }}
            onMouseEnter={(e) => {
              if (menuOpen) return;
              const r = e.currentTarget.getBoundingClientRect();
              const left = Math.max(8, Math.min(r.left, window.innerWidth - 220));
              const top = Math.min(r.bottom + 6, window.innerHeight - 80);
              setHover({ left, top, c: p.c });
            }}
            onMouseLeave={() => setHover(null)}
          >
            <span
              className="gg-subject"
              style={{
                left: msgX,
                width: Math.max(0, containerW - msgX - 12),
                lineHeight: `${ROW_H}px`,
              }}
            >
              {p.c.subject}
            </span>
          </div>
        );
      })}
      {hover && (
        <div className="gg-tip" style={{ left: hover.left, top: hover.top }}>
          <span className="gg-tip-hash" style={{ color: hoverColor }}>{hover.c.hash.slice(0, 7)}</span>
          <span className="gg-tip-subject">{clip(hover.c.subject, 34)}</span>
          <span className="gg-tip-meta">{hover.c.author} · {fmtTsFull(hover.c.ts)}</span>
        </div>
      )}
    </div>
  );
}
