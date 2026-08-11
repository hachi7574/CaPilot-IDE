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

export function layout(log: GitLogEntry[]): CommitGraphLayout {
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

  // ── Columns: lane walk down the rows ────────────────────────────────────
  // `lanes[col]` is the hash of the commit the column's line currently points
  // to (i.e. the next commit down the graph), or null when the lane is free.
  const colMap = new Map<string, number>();
  const lanes: (string | null)[] = [];

  for (let r = 0; r < log.length; r++) {
    const c = log[r];
    const h = c.hash;
    const incoming: number[] = [];
    for (let i = 0; i < lanes.length; i++) if (lanes[i] === h) incoming.push(i);

    let idx: number;
    if (incoming.length === 0) {
      // New tip (a ref head, or a divergent branch): claim the leftmost free
      // column, or open a fresh one at the right.
      idx = lanes.indexOf(null);
      if (idx === -1) {
        idx = lanes.length;
        lanes.push(null);
      }
    } else {
      idx = incoming[0];
      // Extra lanes that converged here terminate at this row.
      for (const i of incoming.slice(1)) lanes[i] = null;
    }
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
