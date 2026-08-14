/**
 * Self-contained commit-tree layout for the git panel.
 *
 * Pure functions only — no React, no DOM — so the graph geometry can be unit
 * tested with plain `tsx` scripts. We replaced @gitgraph/react with this
 * because the library:
 *   1. hard-codes `author.timestamp = Date.now()` (real dates need a custom
 *      message renderer),
 *   2. drops every commit whose ref is missing (a broken ref query blanks the
 *      whole tree), and
 *   3. can't be restyled — the sidebar layout is at its mercy.
 *
 * The layout is a single pass over the git-log order plus a lane walk
 * (`git log --graph`-style):
 *   - rows   = the git-log order itself: the backend reads
 *              `git log --all --date-order`, so this array is already stacked
 *              newest-on-top with every parent strictly below its children.
 *              Each commit gets its own row at its chronological slot — unlike
 *              compact topological depth, two branch tips do NOT share a row
 *              just because they are both tips.
 *   - columns = a lane walk: each branch keeps its own column; a merge's extra
 *              parents open columns to the right; when several lanes converge
 *              on one commit it takes the leftmost lane and the others merge in.
 *   - main line = when a `currentBranch` is supplied, the first-parent chain
 *              from its tip forms the "main line" and owns the leftmost lane.
 *              Everything else — a branch merely ahead of the current one, a
 *              divergent branch, or a merged-in side branch — keeps its own
 *              lane to the right and reconnects to the trunk via an elbow at
 *              its join point. So the current branch's own history always reads
 *              as a single leftmost spine, with other work fanned out to the
 *              right in the order their tips appear. Linear histories stay one
 *              lane when the current branch's commit is unknown (truncated log
 *              / detached HEAD).
 */

/** One commit from the backend `git_log` command (full hashes + parents + refs). */
export interface GitLogEntry {
  hash: string;
  parents: string[];
  refs: string[];
  subject: string;
  author: string;
  email?: string;
  ts: number;
}

export interface PlacedCommit {
  c: GitLogEntry;
  /** Lane column (0 = leftmost). */
  col: number;
  /** Row: 0 = newest commit on top. */
  row: number;
}

export interface CommitGraphLayout {
  commits: PlacedCommit[];
  /** Number of lane columns the tree occupies. */
  numCols: number;
  /** Largest row index (oldest visible commit). -1 when empty. */
  maxRow: number;
}

/** One live lane column during the walk: the hash of the next commit its line
 *  points to (null = free). */
type Lane = string | null;

export function layout(log: GitLogEntry[], currentBranch?: string): CommitGraphLayout {
  if (log.length === 0) return { commits: [], numCols: 0, maxRow: -1 };

  const byHash = new Map<string, GitLogEntry>();
  log.forEach((c) => byHash.set(c.hash, c));

  // ── Rows = the git-log order itself ─────────────────────────────────────
  // The backend reads `git log --all --date-order`, so this array is already
  // stacked newest-on-top with every parent strictly below its children. Each
  // commit gets its own row at its chronological slot — unlike compact
  // topological depth, two branch tips do NOT land on the same row just because
  // they are both tips.
  const maxRow = log.length - 1;

  // ── Main line = the current branch's first-parent chain ─────────────────
  // The trunk of the graph: commits on it own the leftmost lane, and every
  // other branch is laid out to the right. First-parent (not reachability)
  // keeps the trunk a single thread — a merged-in side branch is NOT part of
  // it, so it gets its own lane instead of trying to share column 0. When the
  // current branch's commit is outside the window (truncated log) or unknown
  // (detached HEAD), every commit counts as main so the whole log stays a
  // single lane — the pre-existing behaviour.
  const onMain = new Set<string>();
  if (currentBranch) {
    const head = log.find((c) => c.refs.includes(currentBranch));
    if (head) {
      let h: string | undefined = head.hash;
      while (h && byHash.has(h)) {
        onMain.add(h);
        h = byHash.get(h)!.parents[0];
      }
    }
  }
  const mainLineActive = onMain.size > 0;

  // ── Columns: lane walk down the rows ────────────────────────────────────
  // `lanes[col]` holds the hash of the next commit the column's line points to
  // (null = free). When a main line is active, column 0 is reserved for it:
  // new main tips claim it directly, and a main commit that resumes from a side
  // lane terminates that lane and re-claims column 0, so the trunk is always
  // the leftmost column. Side tips claim the leftmost free lane strictly to
  // the right of it.
  const colMap = new Map<string, number>();
  const lanes: Lane[] = [];

  for (let r = 0; r < log.length; r++) {
    const c = log[r];
    const h = c.hash;
    const isMain = mainLineActive && onMain.has(h);
    const incoming: number[] = [];
    for (let i = 0; i < lanes.length; i++) if (lanes[i] === h) incoming.push(i);

    let idx: number;
    if (isMain) {
      // The main line owns the leftmost column. Any side lanes that converged
      // on this commit (a branch reaching its join point) end here.
      for (const i of incoming) lanes[i] = null;
      idx = 0;
    } else if (incoming.length > 0) {
      idx = incoming[0];
      // Extra lanes that converged here terminate at this row.
      for (const i of incoming.slice(1)) lanes[i] = null;
    } else if (mainLineActive) {
      // New side tip: keep it right of the trunk. Claim the leftmost free lane
      // from column 1 on, or open a fresh one at the right.
      idx = lanes.findIndex((l, i) => i >= 1 && l === null);
      if (idx === -1) idx = Math.max(1, lanes.length);
    } else {
      // No main line: new tips claim the leftmost free lane (a linear history
      // stays a single lane).
      idx = lanes.findIndex((l) => l === null);
      if (idx === -1) idx = lanes.length;
    }
    // Ensure the column exists (a main commit can claim 0 before any lane, and
    // a side tip may open a lane past a sparse array).
    while (lanes.length <= idx) lanes.push(null);
    colMap.set(h, idx);

    // The commit's first (in-log) parent keeps this lane going down; extra
    // parents open new lanes to the right (unless already tracked elsewhere).
    const inLogParents = c.parents.filter((p) => byHash.has(p));
    lanes[idx] = inLogParents[0] ?? null;
    for (const p of inLogParents.slice(1)) {
      if (lanes.includes(p)) continue;
      lanes.push(p);
    }
    // Tighten from the right so finished lanes don't inflate the width.
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) lanes.pop();
  }

  let numCols = 1;
  for (const col of colMap.values()) if (col + 1 > numCols) numCols = col + 1;

  return {
    commits: log.map((c, i) => ({ c, col: colMap.get(c.hash)!, row: i })),
    numCols,
    maxRow,
  };
}
