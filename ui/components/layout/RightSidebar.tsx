import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { capilotTheme } from "../editor/capilotTheme";
import { MergeView } from "@codemirror/merge";
import { useStore } from "../../state/store";
import type { ContentSearchFileResult, ContentSearchMatch } from "../../state/store";
import { fileTab, isImagePath } from "../../state/openFile";
import { spawnBashAt } from "../../state/agentActions";
import { useFileContentSearch } from "./useFileContentSearch";
import { TodoPanel } from "./TodoPanel";
import { CommitGraph, type GitLogEntry } from "./CommitGraph";
import { Icon } from "../Icon";

type RightTab = "overview" | "files" | "git";

export function RightSidebar() {
  const [activeTab, setActiveTab] = useState<RightTab>("overview");
  const rightWidth = useStore((s) => s.rightWidth);
  const setRightWidth = useStore((s) => s.setRightWidth);
  const rightSidebarOpen = useStore((s) => s.rightSidebarOpen);
  const toggleRightSidebar = useStore((s) => s.toggleRightSidebar);
  // 概览 scope (global ↔ focused project), toggled by the overview tab button.
  const todoScope = useStore((s) => s.todoScope);
  const toggleTodoScope = useStore((s) => s.toggleTodoScope);
  // Resize-handle mousedown position: distinguishes a click (toggle) from a
  // drag (resize) on the divider.
  const resizeStartRef = useRef<{ x: number; y: number } | null>(null);

  // Draggable right sidebar resize. In the current layout this panel is the
  // leftmost one (rendered before the main area), with the handle on its right
  // edge: dragging rightward grows it.
  const startRightResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = rightWidth;
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(520, Math.max(260, startWidth + (ev.clientX - startX)));
      setRightWidth(w);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <>
      <div
        className={`right-sidebar${!rightSidebarOpen ? " collapsed" : ""}`}
        style={rightSidebarOpen ? { width: rightWidth } : undefined}
      >
        {/* Tabs */}
        <div className="right-tabs">
          <div
            className={`right-tab${activeTab === "overview" ? " active" : ""}${todoScope === "project" ? " overview-project" : ""}`}
            onClick={() => {
              // Already on 概览? Clicking the tab again toggles the view scope
              // (global ↔ current project); otherwise open 概览 first.
              if (activeTab === "overview") toggleTodoScope();
              else setActiveTab("overview");
            }}
            title={`概览 · ${todoScope === "global" ? "全局" : "当前项目"}`}
          >
            <Icon name="activity" size={15} />
          </div>
          <div
            className={`right-tab${activeTab === "files" ? " active" : ""}`}
            onClick={() => setActiveTab("files")}
            title="文件"
          >
            <Icon name="file-text" size={15} />
          </div>
          <div
            className={`right-tab${activeTab === "git" ? " active" : ""}`}
            onClick={() => setActiveTab("git")}
            title="Git"
          >
            <Icon name="git" size={15} />
          </div>
        </div>

        {/* Tab Content */}
        <div className="right-panel">
          {activeTab === "overview" && <TodoPanel />}
          {activeTab === "files" && <FilesPanel />}
          {activeTab === "git" && <GitPanel />}
        </div>
      </div>
      {/* Resize handle: drag to resize; the hover button collapses/expands the
          sidebar (always rendered so a collapsed sidebar can be reopened). */}
      <div
        className="resize-handle"
        id="resize-right"
        onMouseDown={(e) => {
          resizeStartRef.current = { x: e.clientX, y: e.clientY };
          startRightResize(e);
        }}
        onClick={(e) => {
          // A click (no movement) toggles the sidebar; a drag resizes instead.
          const start = resizeStartRef.current;
          resizeStartRef.current = null;
          if (
            start &&
            Math.hypot(e.clientX - start.x, e.clientY - start.y) > 5
          ) {
            return;
          }
          toggleRightSidebar();
        }}
      >
        <button
          className="resize-collapse"
          title={rightSidebarOpen ? "收起侧栏" : "展开侧栏"}
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            toggleRightSidebar();
          }}
        >
          <Icon name={rightSidebarOpen ? "chevron-left" : "chevron-right"} size={10} />
        </button>
      </div>
    </>
  );
}

/**
 * Root directory for the file tree / git panel.
 *
 * Prefers the focused project's root (`store.focusedProject` + its entry in
 * `store.projectRoots`), so both panels follow the project focused in the left
 * sidebar. Falls back to the active tab's agent cwd, then the workspace root.
 */
function useProjectRoot(): string {
  const agents = useStore((s) => s.agents);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const focusedProject = useStore((s) => s.focusedProject);
  const projectRoots = useStore((s) => s.projectRoots);
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const cwd = activeTab?.agentId ? agents.get(activeTab.agentId)?.cwd : undefined;
  const [fallback, setFallback] = useState("/tmp");
  useEffect(() => {
    invoke<string>("workspace_root")
      .then(setFallback)
      .catch(() => {});
  }, []);
  // Focused project's root (e.g. a git-cloned / local-folder project) wins.
  const focusedRoot = focusedProject ? projectRoots[focusedProject] : undefined;
  if (focusedRoot) return focusedRoot;
  return cwd || fallback;
}

/* ── Files Panel ──────────────────────────────────────────────── */

interface FsEntry {
  name: string;
  is_dir: boolean;
  executable?: boolean;
}

/** A file's git state in the tree: new (untracked) vs modified (tracked change),
 *  plus its staged+unstaged ±N line counts. Absent = clean file. */
interface GitFileState {
  status: "new" | "mod";
  add: number;
  del: number;
}

/* ── Content-search result rendering ─────────────────────────── */

type SearchRow =
  | { kind: "file"; file: ContentSearchFileResult }
  | { kind: "match"; file: ContentSearchFileResult; match: ContentSearchMatch };

/** Flatten results into a virtual-list row stream; collapsed files skip their
 *  match rows (the file header stays). */
function buildSearchRows(
  files: ContentSearchFileResult[],
  collapsed: Record<string, boolean>
): SearchRow[] {
  const rows: SearchRow[] = [];
  for (const file of files) {
    rows.push({ kind: "file", file });
    if (collapsed[file.filePath]) continue;
    for (const match of file.matches) rows.push({ kind: "match", file, match });
  }
  return rows;
}

/** Split a match's line into before/hit/after for highlighting, trimming the
 *  pre/post text to fit the narrow sidebar while keeping the match in view.
 *  Slices by code points so CJK offsets from the backend line up. */
function splitMatch(match: ContentSearchMatch): { before: string; hit: string; after: string } {
  const content = match.lineContent;
  const col = match.displayColumn ?? match.column;
  const len = match.displayMatchLength ?? match.matchLength;
  const chars = Array.from(content);
  const end = Math.min(chars.length, col + len);
  const beforeMax = 40;
  const afterMax = 60;
  if (col <= beforeMax && chars.length - end <= afterMax) {
    return {
      before: chars.slice(0, col).join(""),
      hit: chars.slice(col, end).join(""),
      after: chars.slice(end).join(""),
    };
  }
  const start = Math.max(0, Math.min(col - beforeMax, chars.length - (beforeMax + afterMax)));
  const stop = Math.min(chars.length, start + beforeMax + afterMax);
  return {
    before: (start > 0 ? "…" : "") + chars.slice(start, col).join(""),
    hit: chars.slice(col, end).join(""),
    after: chars.slice(end, stop).join("") + (stop < chars.length ? "…" : ""),
  };
}

const RESULT_ROW_H = 24;

/** Windowed (virtualized) result list — a big match set never renders all rows
 *  at once on WebKitGTK. Fixed row height keeps index↔offset arithmetic cheap. */
function SearchResultsList({
  files,
  collapsed,
  onToggleFile,
  onOpenMatch,
}: {
  files: ContentSearchFileResult[];
  collapsed: Record<string, boolean>;
  onToggleFile: (path: string) => void;
  onOpenMatch: (file: ContentSearchFileResult, match: ContentSearchMatch) => void;
}) {
  const rows = useMemo(() => buildSearchRows(files, collapsed), [files, collapsed]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(300);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const totalH = rows.length * RESULT_ROW_H;
  const overscan = 12;
  const start = Math.max(0, Math.floor(scrollTop / RESULT_ROW_H) - overscan);
  const end = Math.min(rows.length, Math.ceil((scrollTop + viewH) / RESULT_ROW_H) + overscan);
  const visible = rows.slice(start, end);

  return (
    <div
      className="files-results"
      ref={scrollRef}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
    >
      <div style={{ height: totalH, position: "relative" }}>
        {visible.map((row, i) => {
          const top = (start + i) * RESULT_ROW_H;
          return (
            <div
              key={row.kind === "file" ? row.file.filePath : `${row.file.filePath}:${row.match.line}:${start + i}`}
              className={`files-result-row ${row.kind === "file" ? "is-file" : "is-match"}`}
              style={{ position: "absolute", top, left: 0, right: 0, height: RESULT_ROW_H }}
              onClick={() =>
                row.kind === "file" ? onToggleFile(row.file.filePath) : onOpenMatch(row.file, row.match)
              }
              title={row.kind === "file" ? row.file.relativePath : `${row.file.relativePath}:${row.match.line}`}
            >
              {row.kind === "file" ? (
                <>
                  <span className="fres-chev">{collapsed[row.file.filePath] ? "▸" : "▾"}</span>
                  <span className="fres-path">{row.file.relativePath}</span>
                  <span className="fres-count">{row.file.matchCount ?? row.file.matches.length}</span>
                </>
              ) : (
                <>
                  <span className="fres-line">{row.match.line}</span>
                  <span className="fres-text">
                    {(() => {
                      const seg = splitMatch(row.match);
                      return (
                        <>
                          <span className="fres-pre">{seg.before}</span>
                          <mark className="fres-hit">{seg.hit}</mark>
                          <span className="fres-post">{seg.after}</span>
                        </>
                      );
                    })()}
                  </span>
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const SKIP_DIRS = new Set([".git", "node_modules", "target", ".claude", "dist", "build"]);

/** web/html files → warn (.file-web), config files → success (.rtx-file-conf),
 *  images → image icon (.file-image). */
function fileClass(name: string): { cls: string; icon: string } {
  const isWeb = name === "index.html" || /\.html?$/.test(name);
  const isConf = name === "tauri.conf.json" || /\.(conf|config)\.json$/.test(name);
  if (isWeb) return { cls: "file file-web", icon: "globe" };
  if (isConf) return { cls: "file rtx-file-conf", icon: "file-text" };
  if (isImagePath(name)) return { cls: "file file-image", icon: "image" };
  return { cls: "file", icon: "file-text" };
}

/** Single-quote a path for a bash command line (handles embedded single quotes). */
function shq(p: string): string {
  return `'${p.replace(/'/g, `'\\''`)}'`;
}

/** Resolve the project that owns an absolute path: focused project wins, then
 *  any project whose recorded root is a path prefix, then the `workspaces/<name>`
 *  segment, then the path's base name. */
function projectForPath(path: string): string {
  const s = useStore.getState();
  const roots = s.projectRoots;
  const under = (root: string | undefined) =>
    !!root && (path === root || path.startsWith(root.endsWith("/") ? root : root + "/"));
  if (s.focusedProject && under(roots[s.focusedProject])) return s.focusedProject;
  for (const [name, root] of Object.entries(roots)) {
    if (under(root)) return name;
  }
  const m = path.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] || "default";
}

/** Build the shell command that runs a file from its own directory (cwd = dir),
 *  or null when the file isn't runnable. Uses the file's relative `./name`. */
function runCommandFor(e: FsEntry): string | null {
  const dot = e.name.lastIndexOf(".");
  const ext = dot >= 0 ? e.name.slice(dot).toLowerCase() : "";
  const rel = `./${e.name}`;
  const q = shq(rel);
  switch (ext) {
    case ".py": return `python3 ${q}`;
    case ".sh":
    case ".bash": return `bash ${q}`;
    case ".js":
    case ".mjs":
    case ".cjs": return `node ${q}`;
    case ".ts":
    case ".tsx": return `npx tsx ${q}`;
    case ".rb": return `ruby ${q}`;
    case ".php": return `php ${q}`;
    case ".pl": return `perl ${q}`;
    case ".go": return `go run ${q}`;
    default: return e.executable ? q : null;
  }
}

/** Copy text to the OS clipboard (navigator API with execCommand fallback). */
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // WebKit webviews may reject the async API — fall back to a hidden textarea.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    document.body.removeChild(ta);
  }
}

function FilesPanel() {
  const root = useProjectRoot();
  const [dirs, setDirs] = useState<Map<string, FsEntry[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  // The Files search box filters by *name* by default; toggling to 内容 runs a
  // backend content search (`fs_search`) whose results replace the tree.
  const [searchMode, setSearchMode] = useState<"name" | "content">("name");
  const contentSearch = useFileContentSearch(root);
  const fs = contentSearch.state;
  const searchInputRef = useRef<HTMLInputElement>(null);
  // Focusing the input on mode switch keeps the flow keyboard-first; entering
  // content mode with a persisted query re-runs it so results are fresh.
  useEffect(() => {
    if (searchMode === "content") {
      searchInputRef.current?.focus();
      if (fs.query.trim() !== "") contentSearch.submit();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchMode]);
  const [creating, setCreating] = useState<{ dir: string; kind: "file" | "dir" } | null>(null);
  const [newName, setNewName] = useState("");
  const [createError, setCreateError] = useState("");
  // Right-click context menu: target kind + cursor position.
  const [menu, setMenu] = useState<{ x: number; y: number; kind: "file" | "dir" | "space"; path?: string } | null>(null);
  // App-internal clipboard (VS Code style): a single cut/copy source.
  const [clip, setClip] = useState<{ path: string; mode: "copy" | "cut" } | null>(null);
  // Transient operation result shown at the bottom of the panel.
  const [notice, setNotice] = useState<{ text: string; err?: boolean } | null>(null);
  // Inline rename: path of the entry being renamed + the input value.
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [renameError, setRenameError] = useState("");
  // Single-click select target in the tree; second click / double-click opens.
  // `isDir` lets keyboard actions (paste-into, delete, rename) target folders.
  const [selected, setSelected] = useState<{ path: string; isDir: boolean } | null>(null);
  // Per-file git state (new/modified + ±N) for the tree, keyed by absolute path.
  const [gitState, setGitState] = useState<Record<string, GitFileState>>({});
  const addTab = useStore((s) => s.addTab);
  const closeTab = useStore((s) => s.closeTab);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const rightSidebarOpen = useStore((s) => s.rightSidebarOpen);
  // Exactly one open plain shell (OS shell / bash; agent sessions excluded) →
  // the "open in current terminal" folder action becomes available.
  const bashIds = tabs
    .filter((t) => t.type === "agent" && t.agentId)
    .map((t) => t.agentId!)
    .filter((id) => {
      const rt = agents.get(id)?.runtime ?? "";
      return rt === "shell" || rt.startsWith("bash");
    });
  const singleBashId = bashIds.length === 1 ? bashIds[0] : null;

  useEffect(() => {
    loadChildren(root);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  const loadChildren = async (dir: string) => {
    try {
      const list = await invoke<FsEntry[]>("fs_list", { dir });
      setDirs((prev) => new Map(prev).set(dir, list));
    } catch {
      setDirs((prev) => new Map(prev).set(dir, []));
    }
  };

  /** Recursively load the whole tree so the search filter can match deep files. */
  const loadTree = async (dir: string) => {
    const visited = new Set<string>();
    const stack = [dir];
    while (stack.length) {
      const d = stack.pop()!;
      if (visited.has(d)) continue;
      visited.add(d);
      try {
        const list = await invoke<FsEntry[]>("fs_list", { dir: d });
        setDirs((prev) => new Map(prev).set(d, list));
        for (const e of list) {
          if (e.is_dir && !SKIP_DIRS.has(e.name)) stack.push(`${d}/${e.name}`);
        }
      } catch {
        // Unreadable directory — skip.
      }
    }
  };

  useEffect(() => {
    if (filter.trim() === "") return;
    const t = setTimeout(() => loadTree(root), 200);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filter, root]);

  // Auto-refresh: while the Files tab is mounted, re-check the cached directory
  // listings every 2 s and update only the ones whose on-disk listing actually
  // changed. The map keeps its size (entries replaced in place, never grown) and
  // a no-change tick returns the same state reference, so there is no re-render
  // churn or unbounded memory use. A `running` guard keeps slow ticks from
  // overlapping.
  const dirsRef = useRef(dirs);
  dirsRef.current = dirs;
  useEffect(() => {
    let running = false;
    const t = setInterval(async () => {
      if (running) return;
      running = true;
      const snapshot = [...dirsRef.current.keys()];
      try {
        for (const d of snapshot) {
          let fresh: FsEntry[] = [];
          try {
            fresh = await invoke<FsEntry[]>("fs_list", { dir: d });
          } catch {
            fresh = [];
          }
          setDirs((prev) => {
            const cur = prev.get(d);
            if (
              cur &&
              cur.length === fresh.length &&
              cur.every(
                (e, i) =>
                  e.name === fresh[i].name &&
                  e.is_dir === fresh[i].is_dir &&
                  e.executable === fresh[i].executable
              )
            ) {
              return prev;
            }
            return new Map(prev).set(d, fresh);
          });
        }
      } finally {
        running = false;
      }
    }, 2000);
    return () => clearInterval(t);
  }, []);

  // Git status for the tree: poll git_status for the project root and cache
  // per-path status + line counts. New (untracked) files get the green class,
  // modified files the yellow class, and both show their ±N badge — the diff
  // feedback that used to live on the editor tab. Same cost discipline as the
  // Git panel: skip while hidden / collapsed, don't overlap ticks, and only
  // re-set state when the map actually changed (a status with no entry for a
  // path drops its entry, e.g. after a commit). Follows GitPanel's convention:
  // `root` is treated as the repo root.
  const gitStateRef = useRef(gitState);
  gitStateRef.current = gitState;
  const rightSidebarOpenRef = useRef(rightSidebarOpen);
  rightSidebarOpenRef.current = rightSidebarOpen;
  useEffect(() => {
    let running = false;
    const tick = async () => {
      if (running) return;
      if (document.visibilityState === "hidden") return;
      if (!rightSidebarOpenRef.current) return;
      running = true;
      try {
        const entries: GitEntry[] =
          (await invoke<GitEntry[]>("git_status", { dir: root })) ?? [];
        const base = root.endsWith("/") ? root : `${root}/`;
        const next: Record<string, GitFileState> = {};
        for (const e of entries) {
          next[base + e.path] = {
            status: e.index === "?" && e.worktree === "?" ? "new" : "mod",
            add: e.add,
            del: e.del,
          };
        }
        if (JSON.stringify(gitStateRef.current) !== JSON.stringify(next)) {
          gitStateRef.current = next;
          setGitState(next);
        }
      } catch {
        // Not a git repo (or unreadable) — the tree renders plain colors.
        if (Object.keys(gitStateRef.current).length > 0) {
          gitStateRef.current = {};
          setGitState({});
        }
      } finally {
        running = false;
      }
    };
    const t = setInterval(() => void tick(), 2000);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  // Aggregate file git state up to ancestor directories so a folder reads as
  // "new" (green) when it only holds untracked files, or "modified" (yellow)
  // when it holds any tracked change. Modified wins over new. The root itself
  // is never marked.
  const dirGit = useMemo(() => {
    const hasMod = new Set<string>();
    const hasNew = new Set<string>();
    const base = root.endsWith("/") ? root : `${root}/`;
    for (const [p, st] of Object.entries(gitState)) {
      let dir = p.slice(0, p.lastIndexOf("/"));
      while (dir.startsWith(base)) {
        if (st.status === "mod") hasMod.add(dir);
        else hasNew.add(dir);
        dir = dir.slice(0, dir.lastIndexOf("/"));
      }
    }
    return { hasMod, hasNew };
  }, [gitState, root]);

  // Close the context menu on any click / new right-click / Escape. The
  // timestamp guard ignores events fired within 150 ms of opening — some
  // compositors synthesize a click right after the contextmenu, which would
  // otherwise close the menu instantly.
  useEffect(() => {
    if (!menu) return;
    const openedAt = Date.now();
    const close = () => {
      if (Date.now() - openedAt > 150) setMenu(null);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  // Auto-dismiss the operation notice after 3 s.
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), 3000);
    return () => clearTimeout(t);
  }, [notice]);

  const toggleDir = (dir: string) => {
    if (expanded.has(dir)) {
      setExpanded((prev) => {
        const next = new Set(prev);
        next.delete(dir);
        return next;
      });
    } else {
      setExpanded((prev) => new Set(prev).add(dir));
      loadChildren(dir);
    }
  };

  const openFile = (path: string, name: string) => {
    addTab(fileTab(path, name));
  };

  const clickFile = (path: string, name: string) => {
    // First click selects; clicking the already-selected file opens it.
    if (selected?.path === path && !selected.isDir) openFile(path, name);
    else setSelected({ path, isDir: false });
  };

  const startCreate = (dir: string, kind: "file" | "dir") => {
    // Keep the target visible so the inline input renders under the right dir.
    setExpanded((prev) => new Set(prev).add(dir));
    setCreating({ dir, kind });
    setNewName("");
    setCreateError("");
  };

  const cancelCreate = () => {
    setCreating(null);
    setNewName("");
    setCreateError("");
  };

  const submitCreate = async () => {
    if (!creating) return;
    const name = newName.trim();
    if (!name) return;
    if (name.includes("/") || name === "." || name === "..") {
      setCreateError("名称不能包含 / 或为 . / ..");
      return;
    }
    const path = `${creating.dir}/${name}`;
    try {
      if (creating.kind === "file") {
        await invoke("fs_create_file", { path });
        loadChildren(creating.dir);
        openFile(path, name);
      } else {
        await invoke("fs_create_dir", { path });
        loadChildren(creating.dir);
      }
      setCreating(null);
      setNewName("");
      setCreateError("");
    } catch (err) {
      setCreateError(String(err));
    }
  };

  const openCtx = (e: React.MouseEvent, kind: "file" | "dir" | "space", path?: string) => {
    e.preventDefault();
    e.stopPropagation();
    // Clamp so the ~190px menu stays inside the viewport.
    const x = Math.min(e.clientX, window.innerWidth - 200);
    const y = Math.min(e.clientY, window.innerHeight - 140);
    setMenu({ x, y, kind, path });
  };

  const closeMenu = () => setMenu(null);

  /** Directory the current menu action operates on (paste / create targets). */
  const menuDir = () => {
    if (!menu) return root;
    if (menu.kind !== "file") return menu.path || root;
    const p = menu.path || "";
    return p.slice(0, p.lastIndexOf("/")) || root;
  };

  const doCopy = () => {
    if (!menu?.path) return;
    setClip({ path: menu.path, mode: "copy" });
    closeMenu();
  };

  const doCut = () => {
    if (!menu?.path) return;
    setClip({ path: menu.path, mode: "cut" });
    closeMenu();
  };

  const doPaste = async () => {
    if (!clip) return;
    const dest = menuDir();
    const isMove = clip.mode === "cut";
    try {
      const created = await invoke<string>("fs_paste", { src: clip.path, destDir: dest, isMove });
      const name = created.slice(created.lastIndexOf("/") + 1);
      setNotice({ text: `已${isMove ? "移动" : "复制"}为 ${name}` });
      loadChildren(dest);
      const srcParent = clip.path.slice(0, clip.path.lastIndexOf("/"));
      if (srcParent !== dest) loadChildren(srcParent);
      if (isMove) setClip(null);
    } catch (err) {
      setNotice({ text: String(err), err: true });
    }
    closeMenu();
  };

  /** cd the single open bash terminal to a folder (req: open in current terminal). */
  const doOpenInCurrentTerminal = () => {
    if (!menu?.path || !singleBashId) return;
    invoke("agent_write", {
      id: singleBashId,
      data: `cd ${shq(menu.path)}`,
      raw: false,
    }).catch(() => {});
    setActiveTab(singleBashId);
    closeMenu();
  };

  /** Spawn a new bash terminal rooted at a folder. */
  const doOpenInNewTerminal = () => {
    if (!menu?.path) return;
    const proj = projectForPath(menu.path);
    spawnBashAt(proj, menu.path).catch((e) =>
      setNotice({ text: String(e), err: true })
    );
    closeMenu();
  };

  /** Resolve the runnable command for the menu's file path (null = not runnable). */
  const fileRunCommand = (path: string | undefined): string | null => {
    if (!path) return null;
    const parent = path.slice(0, path.lastIndexOf("/"));
    const entry = dirs.get(parent)?.find((e) => `${parent}/${e.name}` === path);
    return entry ? runCommandFor(entry) : null;
  };

  /** Run a file: reuse the single bash terminal if present (cd + run), else
   *  spawn a fresh bash terminal in the file's directory and run it there. */
  const doRunFile = () => {
    if (!menu?.path) return;
    const cmd = fileRunCommand(menu.path);
    if (!cmd) return;
    const dir = menu.path.slice(0, menu.path.lastIndexOf("/"));
    if (singleBashId) {
      invoke("agent_write", {
        id: singleBashId,
        data: `cd ${shq(dir)} && ${cmd}`,
        raw: false,
      }).catch(() => {});
      setActiveTab(singleBashId);
    } else {
      const proj = projectForPath(dir);
      spawnBashAt(proj, dir, cmd).catch((e) =>
        setNotice({ text: String(e), err: true })
      );
    }
    closeMenu();
  };

  const doNew = (kind: "file" | "dir") => {
    startCreate(menuDir(), kind);
    closeMenu();
  };

  const doCopyPath = () => {
    if (!menu?.path) return;
    copyText(menu.path);
    closeMenu();
  };

  /** Delete a file/dir after confirmation. Shared by the context menu and the
   *  Del keyboard shortcut. Clears the tree selection when it targeted `path`. */
  const deletePath = async (path: string) => {
    const name = path.slice(path.lastIndexOf("/") + 1);
    const ok = await confirm(`确定删除「${name}」？此操作不可撤销。`, {
      title: "删除",
      kind: "warning",
    });
    if (!ok) return;
    const parent = path.slice(0, path.lastIndexOf("/"));
    try {
      await invoke("fs_delete", { path });
      loadChildren(parent);
      if (selected?.path === path) setSelected(null);
      setNotice({ text: `已删除 ${name}` });
    } catch (err) {
      setNotice({ text: String(err), err: true });
    }
  };

  const doDelete = () => {
    if (!menu?.path) return;
    closeMenu();
    void deletePath(menu.path);
  };

  const startRename = (path: string) => {
    setRenaming(path);
    setRenameValue(path.slice(path.lastIndexOf("/") + 1));
    setRenameError("");
    setMenu(null);
  };

  const cancelRename = () => {
    setRenaming(null);
    setRenameValue("");
    setRenameError("");
  };

  const submitRename = async () => {
    if (!renaming) return;
    const name = renameValue.trim();
    if (!name || name.includes("/") || name === "." || name === "..") {
      setRenameError("名称不能为空、包含 / 或为 . / ..");
      return;
    }
    const parent = renaming.slice(0, renaming.lastIndexOf("/"));
    try {
      const newPath = await invoke<string>("fs_rename", { src: renaming, newName: name });
      // Drop stale cached children of a renamed dir, then re-show it expanded.
      setDirs((prev) => {
        const next = new Map(prev);
        for (const k of prev.keys()) {
          if (k === renaming || k.startsWith(`${renaming}/`)) next.delete(k);
        }
        return next;
      });
      setExpanded((prev) => {
        const next = new Set(prev);
        for (const k of prev) {
          if (k === renaming || k.startsWith(`${renaming}/`)) next.delete(k);
        }
        next.add(newPath);
        return next;
      });
      loadChildren(parent);
      loadChildren(newPath);
      // Keep the tree selection pointing at the renamed entry.
      if (selected?.path === renaming) {
        setSelected({ path: newPath, isDir: selected.isDir });
      }
      // If the renamed file is open in an editor tab, point it at the new path.
      if (useStore.getState().tabs.some((t) => t.id === `file:${renaming}`)) {
        closeTab(`file:${renaming}`);
        addTab(fileTab(newPath, name));
      }
      setRenaming(null);
      setRenameValue("");
      setRenameError("");
      setNotice({ text: `已重命名为 ${name}` });
    } catch (err) {
      setRenameError(String(err));
    }
  };

  /** Paste destination for keyboard paste: a selected folder, a selected file's
   *  parent directory, or the tree root when nothing is selected. */
  const pasteTarget = (): string => {
    if (!selected) return root;
    if (selected.isDir) return selected.path;
    return selected.path.slice(0, selected.path.lastIndexOf("/")) || root;
  };

  /** Cmd/Ctrl+V: paste the app-internal clipboard into the selection target. */
  const doPasteKeyboard = async () => {
    if (!clip) return;
    const dest = pasteTarget();
    const isMove = clip.mode === "cut";
    try {
      const created = await invoke<string>("fs_paste", { src: clip.path, destDir: dest, isMove });
      const name = created.slice(created.lastIndexOf("/") + 1);
      setNotice({ text: `已${isMove ? "移动" : "复制"}为 ${name}` });
      loadChildren(dest);
      const srcParent = clip.path.slice(0, clip.path.lastIndexOf("/"));
      if (srcParent !== dest) loadChildren(srcParent);
      if (isMove) setClip(null);
    } catch (err) {
      setNotice({ text: String(err), err: true });
    }
    closeMenu();
  };

  // File-system keyboard shortcuts (modifiers follow the platform: Ctrl on
  // Win/Linux, Cmd on macOS): Del 删除 / F2 重命名 / Cmd/Ctrl+C 复制 / V 粘贴 /
  // X 剪切. They operate on the currently selected file/folder. Keys typed into
  // an input / textarea (rename, search, editor) are left alone.
  useEffect(() => {
    const isMac = navigator.platform?.toLowerCase().includes("mac");
    const onKeyDown = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)) {
        return;
      }
      const mod = e.metaKey || e.ctrlKey;
      const k = e.key.toLowerCase();
      if (mod && !e.shiftKey && !e.altKey && k === "c") {
        if (!selected) return;
        e.preventDefault();
        closeMenu();
        setClip({ path: selected.path, mode: "copy" });
        setNotice({ text: `已复制 ${selected.path.slice(selected.path.lastIndexOf("/") + 1)}` });
      } else if (mod && !e.shiftKey && !e.altKey && k === "x") {
        if (!selected) return;
        e.preventDefault();
        closeMenu();
        setClip({ path: selected.path, mode: "cut" });
        setNotice({ text: `已剪切 ${selected.path.slice(selected.path.lastIndexOf("/") + 1)}` });
      } else if (mod && !e.shiftKey && !e.altKey && k === "v") {
        if (!clip) return;
        e.preventDefault();
        void doPasteKeyboard();
      } else if (e.key === "Delete" || (isMac && mod && e.key === "Backspace")) {
        if (!selected) return;
        e.preventDefault();
        closeMenu();
        void deletePath(selected.path);
      } else if (e.key === "F2") {
        if (!selected) return;
        e.preventDefault();
        closeMenu();
        startRename(selected.path);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, clip, root]);

  const isFiltering = filter.trim() !== "";
  const q = filter.trim().toLowerCase();

  const renderEntries = (dir: string, depth: number): React.ReactNode => {
    const entries = (dirs.get(dir) || [])
      .filter((e) => !SKIP_DIRS.has(e.name))
      // While filtering, keep every directory visible so matching files deep
      // down stay reachable; files are matched by name.
      .filter((e) => q === "" || e.is_dir || e.name.toLowerCase().includes(q))
      .sort((a, b) => (a.is_dir === b.is_dir ? a.name.localeCompare(b.name) : a.is_dir ? -1 : 1));
    return (
      <>
        {entries.map((e) => {
          const path = `${dir}/${e.name}`;
          const isCutSource = clip?.mode === "cut" && clip.path === path;
          if (renaming === path) {
            return (
              <div key={path}>
                <div className="files-new" style={{ paddingLeft: depth * 14 }}>
                  <span>{e.is_dir ? <Icon name="folder" size={14} /> : <Icon name={fileClass(e.name).icon} size={14} />}</span>
                  <input
                    autoFocus
                    value={renameValue}
                    onChange={(ev) => {
                      setRenameValue(ev.target.value);
                      setRenameError("");
                    }}
                    onFocus={(ev) => ev.target.select()}
                    onKeyDown={(ev) => {
                      if (ev.key === "Enter") {
                        ev.preventDefault();
                        submitRename();
                      } else if (ev.key === "Escape") {
                        cancelRename();
                      }
                    }}
                    onBlur={cancelRename}
                  />
                </div>
                {renameError && (
                  <div className="files-new-error" style={{ paddingLeft: (depth + 1) * 14 }}>
                    {renameError}
                  </div>
                )}
              </div>
            );
          }
          if (e.is_dir) {
            const open = isFiltering || expanded.has(path);
            const gCls = dirGit.hasMod.has(path)
              ? " dir-mod"
              : dirGit.hasNew.has(path)
                ? " dir-new"
                : "";
            return (
              <div key={path}>
                <div
                  className={`dir${gCls}${isCutSource ? " files-ctx-cut" : ""}${selected?.path === path ? " selected" : ""}`}
                  style={{ paddingLeft: depth * 14 }}
                  onClick={() => {
                    toggleDir(path);
                    setSelected({ path, isDir: true });
                  }}
                  onContextMenu={(ev) => {
                    setSelected({ path, isDir: true });
                    openCtx(ev, "dir", path);
                  }}
                  draggable
                  onDragStart={(ev) => {
                    ev.dataTransfer.setData("text/plain", path);
                    ev.dataTransfer.effectAllowed = "copy";
                  }}
                  title={path}
                >
                  <span>{open ? <Icon name="chevron-down" size={12} /> : <Icon name="chevron-right" size={12} />} <Icon name="folder" size={14} /></span>
                  <span className="dir-label">{e.name}</span>
                  <span
                    className="dir-actions"
                    onClick={(ev) => ev.stopPropagation()}
                  >
                    <button
                      title="新建文件"
                      onClick={() => startCreate(path, "file")}
                    >
                      <Icon name="file-plus" size={14} />
                    </button>
                    <button
                      title="新建文件夹"
                      onClick={() => startCreate(path, "dir")}
                    >
                      <Icon name="folder-plus" size={14} />
                    </button>
                  </span>
                </div>
                {open && renderEntries(path, depth + 1)}
              </div>
            );
          }
          const { cls, icon } = fileClass(e.name);
          const g = gitState[path];
          const gCls = g ? (g.status === "new" ? " file-new" : " file-mod") : "";
          return (
            <div
              key={path}
              className={`${cls}${gCls}${isCutSource ? " files-ctx-cut" : ""}${selected?.path === path ? " selected" : ""}`}
              style={{ paddingLeft: depth * 14 }}
              onClick={() => clickFile(path, e.name)}
              onDoubleClick={() => openFile(path, e.name)}
              onContextMenu={(ev) => {
                setSelected({ path, isDir: false });
                openCtx(ev, "file", path);
              }}
              title={path}
              draggable
              onDragStart={(ev) => {
                ev.dataTransfer.setData("text/plain", path);
                ev.dataTransfer.effectAllowed = "copy";
              }}
            >
              <Icon name={icon} size={14} />
              <span className="file-label">{e.name}</span>
              {g && (g.add > 0 || g.del > 0) && (
                <span className="fs-stats">
                  {g.add > 0 && <span className="fs-add">+{g.add}</span>}
                  {g.del > 0 && <span className="fs-del">−{g.del}</span>}
                </span>
              )}
            </div>
          );
        })}
        {creating && creating.dir === dir && (
          <div style={{ paddingLeft: (depth + 1) * 14 }}>
            <div className="files-new">
              <span>{creating.kind === "dir" ? <Icon name="folder" size={14} /> : <Icon name="file-text" size={14} />}</span>
              <input
                autoFocus
                value={newName}
                placeholder={creating.kind === "dir" ? "文件夹名…" : "文件名…"}
                onChange={(e) => {
                  setNewName(e.target.value);
                  setCreateError("");
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    submitCreate();
                  } else if (e.key === "Escape") {
                    cancelCreate();
                  }
                }}
                onBlur={cancelCreate}
              />
            </div>
            {createError && <div className="files-new-error">{createError}</div>}
          </div>
        )}
      </>
    );
  };

  return (
    <div
      className="tab-panel"
      id="tab-files"
      style={
        searchMode === "content"
          ? { padding: "8px 0", display: "flex", flexDirection: "column", minHeight: 0 }
          : { padding: "8px 0" }
      }
      onContextMenu={(e) => {
        // Blank-space right-click acts on the root directory: give the menu the
        // root path so the folder-consistent actions (open in terminal, copy
        // path) target the workspace root.
        openCtx(e, "space", root);
        setSelected(null);
      }}
    >
      <div className="files-search">
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <div className="files-search-input">
            <input
              ref={searchInputRef}
              type="text"
              placeholder={searchMode === "content" ? "搜索文件内容…" : "搜索文件名…"}
              value={searchMode === "content" ? fs.query : filter}
              onChange={(e) => {
                const q = e.target.value;
                if (searchMode === "name") {
                  setFilter(q);
                  return;
                }
                // Skip the backend run mid-IME-composition; the commit change
                // (isComposing=false) fires a real search.
                const composing = (e.nativeEvent as InputEvent).isComposing;
                if (composing) contentSearch.setQueryQuiet(q);
                else contentSearch.setQuery(q);
              }}
              onKeyDown={(e) => {
                if (searchMode !== "content" || (e.nativeEvent as KeyboardEvent).isComposing) return;
                if (e.key === "Enter") {
                  e.preventDefault();
                  contentSearch.submit();
                } else if (e.key === "Escape" && fs.query) {
                  e.preventDefault();
                  contentSearch.clear();
                }
              }}
            />
            <Icon name="search" size={14} className="files-search-ic" />
          </div>
        </div>
        <div className="files-search-mode" role="group">
          <button
            className={searchMode === "name" ? "active" : ""}
            onClick={() => setSearchMode("name")}
            title="按文件名过滤（本地）"
          >
            名称
          </button>
          <button
            className={searchMode === "content" ? "active" : ""}
            onClick={() => setSearchMode("content")}
            title="搜索文件内容"
          >
            内容
          </button>
        </div>
        {searchMode === "content" && (
          <div className="files-search-opts">
            <div className="files-search-toggles">
              <button
                className={`fs-tg${fs.caseSensitive ? " on" : ""}`}
                onClick={() => contentSearch.setCaseSensitive(!fs.caseSensitive)}
                title="区分大小写"
              >
                Aa
              </button>
              <button
                className={`fs-tg${fs.wholeWord ? " on" : ""}`}
                onClick={() => contentSearch.setWholeWord(!fs.wholeWord)}
                title="全词匹配"
              >
                W
              </button>
              <button
                className={`fs-tg${fs.useRegex ? " on" : ""}`}
                onClick={() => contentSearch.setUseRegex(!fs.useRegex)}
                title="正则表达式"
              >
                .*
              </button>
            </div>
            <input
              className="fs-filter-input"
              placeholder="包含文件，如 *.ts, src/**"
              value={fs.includePattern}
              onChange={(e) => contentSearch.setIncludePattern(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") contentSearch.submit();
              }}
            />
            <input
              className="fs-filter-input"
              placeholder="排除文件，如 dist, *.min.js"
              value={fs.excludePattern}
              onChange={(e) => contentSearch.setExcludePattern(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") contentSearch.submit();
              }}
            />
          </div>
        )}
      </div>
      {searchMode === "content" ? (
        <div className="files-content-search">
          <div className="files-results-summary">
            {fs.loading
              ? "搜索中…"
              : fs.results
                ? `${fs.results.totalMatches} 处匹配 · ${fs.results.files.length} 个文件${fs.results.truncated ? " · 已截断" : ""}`
                : fs.query.trim() !== ""
                  ? "无结果"
                  : "输入内容以搜索"}
          </div>
          {fs.loading && !fs.results ? (
            <div className="files-results-loading">搜索中…</div>
          ) : fs.results && fs.results.files.length > 0 ? (
            <SearchResultsList
              files={fs.results.files}
              collapsed={fs.collapsed}
              onToggleFile={(p) => contentSearch.toggleCollapsed(p)}
              onOpenMatch={(file, match) =>
                contentSearch.openMatch(file.filePath, match.line, match.column)
              }
            />
          ) : (
            !fs.loading && <div className="files-results-loading">{fs.query.trim() ? "无匹配内容" : ""}</div>
          )}
        </div>
      ) : (
        <div className="files-tree">{renderEntries(root, 0)}</div>
      )}
      {menu && (
        <div
          className="ctx-menu"
          style={{ left: menu.x, top: menu.y }}
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
        >
          {(menu.kind === "dir" || menu.kind === "space") && (
            <>
              {singleBashId && (
                <div className="ctx-item" onClick={doOpenInCurrentTerminal}>
                  <Icon name="folder-open" size={13} /> 在当前终端中打开此文件夹
                </div>
              )}
              <div className="ctx-item" onClick={doOpenInNewTerminal}>
                <Icon name="monitor" size={13} /> 在新终端中打开此文件夹
              </div>
            </>
          )}
          {menu.kind === "file" && fileRunCommand(menu.path) && (
            <div className="ctx-item" onClick={doRunFile}>
              <Icon name="play" size={13} /> 运行此文件
            </div>
          )}
          {menu.kind !== "file" && (
            <>
              <div className="ctx-item" onClick={() => doNew("file")}>
                新建文件
              </div>
              <div className="ctx-item" onClick={() => doNew("dir")}>
                新建文件夹
              </div>
            </>
          )}
          {menu.kind !== "space" ? (
            <>
              <div className="ctx-sep" />
              <div className="ctx-item" onClick={doCopy}>
                复制
              </div>
              <div className="ctx-item" onClick={doCut}>
                剪切
              </div>
              {clip && (
                <div className="ctx-item" onClick={doPaste}>
                  粘贴
                </div>
              )}
              <div className="ctx-sep" />
              <div className="ctx-item" onClick={doCopyPath}>
                复制路径
              </div>
              <div className="ctx-sep" />
              <div className="ctx-item" onClick={() => menu?.path && startRename(menu.path)}>
                <Icon name="pencil" size={13} /> 重命名
              </div>
              <div className="ctx-sep" />
              <div className="ctx-item danger" onClick={doDelete}>
                <Icon name="trash-2" size={13} /> 删除
              </div>
            </>
          ) : (
            <>
              <div className="ctx-sep" />
              <div className="ctx-item" onClick={doCopyPath}>
                复制路径
              </div>
              {clip && (
                <div className="ctx-item" onClick={doPaste}>
                  粘贴
                </div>
              )}
            </>
          )}
        </div>
      )}
      {notice && (
        <div className={notice.err ? "files-notice err" : "files-notice"}>{notice.text}</div>
      )}
    </div>
  );
}

/* ── Git Panel ────────────────────────────────────────────────── */

interface GitEntry {
  index: string;
  worktree: string;
  path: string;
  add: number;
  del: number;
}

interface GitBranch {
  name: string;
  current: boolean;
}

/** Rust `RepoInfo` from `git_repo_info` — whether the root is a git repo. */
interface RepoInfo {
  is_repo: boolean;
  has_remote: boolean;
  branch: string | null;
  /** Commits ahead of upstream → "↑ N 未推送" indicator. */
  ahead: number;
  /** Whether the branch has a configured upstream (else it needs publishing). */
  has_upstream: boolean;
}

/** Status glyph for a single porcelain status char (M/A/D/R/?). */
function glyphFor(code: string): { glyph: string; cls: string } {
  const c = code.trim();
  if (c === "?" || c === "A") return { glyph: "A", cls: "ga" };
  if (c === "M") return { glyph: "M", cls: "gm" };
  if (c === "D") return { glyph: "D", cls: "gd" };
  if (c === "R") return { glyph: "R", cls: "gr" };
  return { glyph: c || "·", cls: "gm" };
}

/** OLD (left) / NEW (right) file content feeding the @codemirror/merge view. */
interface DiffContent {
  old: string;
  new: string;
}

/** One file touched by a commit (`git_show_commit`). */
interface GitFileStat {
  path: string;
  status: string;
  add: number;
  del: number;
}

/** Full commit payload for the "查看提交详情" modal. */
interface GitCommitDetail {
  hash: string;
  subject: string;
  body: string;
  author: string;
  email: string;
  ts: number;
  files: GitFileStat[];
}

/**
 * Inline side-by-side diff powered by `@codemirror/merge`. Mounts a read-only
 * MergeView (OLD left / NEW right) into the container, destroying it on update
 * or unmount so there is never more than one view per container.
 */
function InlineMergeDiff({ oldText, newText }: { oldText: string; newText: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    // Clear any leftover DOM (StrictMode double-mount safety).
    el.textContent = "";
    const readOnlyExt = [
      capilotTheme,
      lineNumbers(),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
    const view = new MergeView({
      a: { doc: oldText, extensions: readOnlyExt },
      b: { doc: newText, extensions: readOnlyExt },
      parent: el,
      orientation: "a-b",
      gutter: true,
      highlightChanges: true,
    });
    return () => {
      view.destroy();
      if (containerRef.current) containerRef.current.textContent = "";
    };
  }, [oldText, newText]);
  return <div className="gv-diff-cm" ref={containerRef} />;
}

function GitPanel() {
  const root = useProjectRoot();
  const rightSidebarOpen = useStore((s) => s.rightSidebarOpen);
  const [repoInfo, setRepoInfo] = useState<RepoInfo | null>(null);
  const [entries, setEntries] = useState<GitEntry[]>([]);
  const [branch, setBranch] = useState("");
  const [branches, setBranches] = useState<GitBranch[]>([]);
  const [log, setLog] = useState<GitLogEntry[]>([]);
  const [logOpen, setLogOpen] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [stagedOpen, setStagedOpen] = useState(true);
  const [changesOpen, setChangesOpen] = useState(true);
  const [diffFor, setDiffFor] = useState<string | null>(null);
  const [diffContent, setDiffContent] = useState<Record<string, DiffContent>>({});
  const [menuOpen, setMenuOpen] = useState(false);
  // Commit-tree right-click menu (commit under cursor + position).
  const [commitMenu, setCommitMenu] = useState<{ x: number; y: number; commit: GitLogEntry } | null>(null);
  // Branch-chip popover (switch / create / pull / push / delete).
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);
  // Inline "新建分支" name input. `{}` = from the chip (create + switch);
  // `{ startAt }` = from a commit (create there, don't switch).
  const [branchPrompt, setBranchPrompt] = useState<{ startAt?: string } | null>(null);
  const [branchName, setBranchName] = useState("");
  const [branchErr, setBranchErr] = useState("");
  // Commit detail modal; "loading" is the transient fetch state.
  const [commitDetail, setCommitDetail] = useState<GitCommitDetail | "loading" | null>(null);
  const addTab = useStore((s) => s.addTab);
  const setActiveTab = useStore((s) => s.setActiveTab);

  const refresh = async () => {
    // Probe git state first: a non-repo dir short-circuits the normal fetches
    // (git_status / branches / log all fail outside a work tree).
    let ri: RepoInfo | null = null;
    try {
      ri = await invoke<RepoInfo>("git_repo_info", { repo: root });
    } catch {
      ri = null;
    }
    setRepoInfo(ri);
    if (!ri?.is_repo) {
      setEntries([]);
      setBranch("");
      setBranches([]);
      setLog([]);
      setError(null);
      setDiffFor(null);
      setDiffContent({});
      return;
    }
    let statusList: GitEntry[] = [];
    try {
      const list = await invoke<GitEntry[]>("git_status", { dir: root });
      statusList = list ?? [];
      setEntries(statusList);
      setError(null);
    } catch (e) {
      setEntries([]);
      setError(String(e));
    }
    try {
      const br = await invoke<string>("git_branch", { repo: root });
      setBranch(br);
    } catch {
      setBranch("");
    }
    try {
      const bl = await invoke<GitBranch[]>("git_branches", { repo: root });
      setBranches(bl ?? []);
    } catch {
      setBranches([]);
    }
    try {
      const lg = await invoke<GitLogEntry[]>("git_log", { repo: root, count: 100 });
      setLog(lg ?? []);
    } catch {
      setLog([]);
    }
    // Keep the open inline diff live across refreshes: re-fetch its content if
    // the file still appears in the status; drop it when the file disappeared
    // (e.g. after a discard or a commit that removed it).
    if (diffFor) {
      const cur = statusList.find((x) => x.path === diffFor);
      if (cur) {
        void loadDiffContent(cur).then((c) =>
          setDiffContent((prev) => ({ ...prev, [diffFor]: c }))
        );
      } else {
        setDiffFor(null);
        setDiffContent({});
      }
    }
  };

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  // Auto-refresh: poll git status while the panel is mounted (i.e. the Git tab
  // is active) so changes agents make on disk appear without a manual ↻ — the
  // key behaviour VS Code gets from its file watcher. A ref guarantees the
  // interval always calls the latest refresh closure (no stale state).
  //
  // Cost control: each refresh spawns ~10-11 git subprocesses (git_repo_info +
  // git_status + git_branches + git_log, plus any open inline diff). To keep
  // that from hammering the disk/CPU:
  //  - interval raised 2.5s → 3s;
  //  - skip ticks while the document is hidden (backgrounded) or the right
  //    sidebar is collapsed (panel not visible), resuming on the next visible
  //    tick;
  //  - a `running` guard keeps slow ticks from overlapping.
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const rightSidebarOpenRef = useRef(rightSidebarOpen);
  rightSidebarOpenRef.current = rightSidebarOpen;
  useEffect(() => {
    let running = false;
    const tick = () => {
      if (running) return;
      if (!rightSidebarOpenRef.current) return;
      if (document.visibilityState === "hidden") return;
      running = true;
      refreshRef
        .current()
        .catch(() => {})
        .finally(() => {
          running = false;
        });
    };
    const t = setInterval(tick, 3000);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") tick();
    });
    return () => {
      clearInterval(t);
      document.removeEventListener("visibilitychange", tick);
    };
  }, []);

  // Close the commit context menu / branch popover on any click, right-click, or
  // Escape (the detail modal closes on Escape too). The timestamp guard ignores
  // events fired within 150 ms of opening — some compositors synthesize a click
  // right after a contextmenu, which would otherwise close the menu instantly.
  useEffect(() => {
    if (!commitMenu && !branchMenuOpen && !commitDetail) return;
    const openedAt = Date.now();
    const close = () => {
      if (Date.now() - openedAt > 150) {
        setCommitMenu(null);
        setBranchMenuOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      setCommitMenu(null);
      setBranchMenuOpen(false);
      setCommitDetail(null);
      setBranchPrompt(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [commitMenu, branchMenuOpen, commitDetail]);

  const runAction = async (fn: () => Promise<unknown>, okMsg: string) => {
    setBusy(true);
    setFeedback(null);
    try {
      await fn();
      setFeedback(okMsg);
      await refresh();
    } catch (e) {
      setFeedback(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Split `git_status` entries into staged (index status) vs changes (worktree
  // status). Untracked (??) rows fall under 更改.
  const staged = entries.filter((e) => e.index !== " " && e.index !== "?");
  const changes = entries.filter((e) => e.worktree !== " ");

  const stageAll = () =>
    runAction(async () => {
      const files = changes.map((e) => e.path);
      if (files.length === 0) return;
      await invoke("git_stage", { repo: root, files });
    }, "已暂存全部变更");

  const unstageAll = () =>
    runAction(async () => {
      const files = staged.map((e) => e.path);
      if (files.length === 0) return;
      await invoke("git_unstage", { repo: root, files });
    }, "已取消暂存全部变更");

  const toggleFile = (e: GitEntry, kind: "staged" | "changes") => {
    const shouldStage = kind === "changes";
    return runAction(async () => {
      await invoke(shouldStage ? "git_stage" : "git_unstage", {
        repo: root,
        files: [e.path],
      });
      setDiffFor(null);
    }, shouldStage ? "已暂存" : "已取消暂存");
  };

  const commit = () =>
    runAction(async () => {
      const m = msg.trim();
      if (!m) throw new Error("请输入 commit message");
      if (staged.length === 0) throw new Error("没有已暂存的更改（请先暂存文件）");
      await invoke("git_commit", { repo: root, message: m });
      setMsg("");
    }, "提交成功");

  /** Commit then push — one action so a local-only commit can't silently stay
   *  off the remote (VS Code's "Commit & Push"). */
  const commitAndPush = () =>
    runAction(async () => {
      const m = msg.trim();
      if (!m) throw new Error("请输入 commit message");
      if (staged.length === 0) throw new Error("没有已暂存的更改（请先暂存文件）");
      await invoke("git_commit", { repo: root, message: m });
      await invoke("git_push", { repo: root });
      setMsg("");
    }, "已提交并推送到远程");

  const pull = () => runAction(() => invoke("git_pull", { repo: root }), "Pull 完成");
  const push = () => runAction(() => invoke("git_push", { repo: root }), "Push 完成");

  // Not-yet-initialized repo → `git init`, then re-probe + load the panel.
  const initRepo = () =>
    runAction(async () => {
      await invoke("git_init", { repo: root });
    }, "已初始化 git 仓库");

  const gitShowSafe = async (rev: string, file: string): Promise<string> => {
    try {
      return await invoke<string>("git_show", { repo: root, file, rev });
    } catch {
      return "";
    }
  };

  const readWorktreeSafe = async (file: string): Promise<string> => {
    const abs = root.endsWith("/") ? `${root}${file}` : `${root}/${file}`;
    try {
      return await invoke<string>("fs_read", { path: abs });
    } catch {
      return "";
    }
  };

  /** Load OLD (left) + NEW (right) content for a file's merge view.
   *  Staged → HEAD vs index (`git show HEAD:<f>` / `git show :0:<f>`);
   *  Untracked → empty vs working tree; Unstaged → index vs working tree. */
  const loadDiffContent = async (e: GitEntry): Promise<DiffContent> => {
    const isStagedRow = e.index !== " " && e.index !== "?";
    const untracked = e.index === "?" && e.worktree === "?";
    let old = "";
    let fresh = "";
    if (isStagedRow) {
      old = await gitShowSafe("HEAD", e.path);
      fresh = await gitShowSafe(":0:", e.path);
    } else if (untracked) {
      old = "";
      fresh = await readWorktreeSafe(e.path);
    } else {
      old = await gitShowSafe(":0:", e.path);
      fresh = await readWorktreeSafe(e.path);
    }
    return { old, new: fresh };
  };

  const toggleDiff = (e: GitEntry) => {
    if (diffFor === e.path) {
      setDiffFor(null);
      return;
    }
    setDiffFor(e.path);
    void loadDiffContent(e).then((c) => {
      setDiffContent((prev) => ({ ...prev, [e.path]: c }));
    });
  };

  const switchBranch = async (name: string) => {
    if (!name || name === branch) return;
    setBusy(true);
    setFeedback(null);
    try {
      await invoke("git_checkout", { repo: root, branch: name });
      setFeedback(`已切换到分支 ${name}`);
    } catch (e) {
      setFeedback(String(e));
    } finally {
      setBusy(false);
    }
    await refresh();
  };

  /** Open the commit-tree right-click menu for a commit row. */
  const openCommitMenu = (e: React.MouseEvent, commit: GitLogEntry) => {
    setBranchMenuOpen(false);
    setBranchPrompt(null);
    setCommitDetail(null);
    // Clamp so the ~220px menu stays inside the viewport.
    const x = Math.min(e.clientX, window.innerWidth - 220);
    const y = Math.min(e.clientY, window.innerHeight - 200);
    setCommitMenu({ x, y, commit });
  };

  /** Copy text to the clipboard, falling back to execCommand (WebKitGTK may not
   *  grant navigator.clipboard outside a secure context). */
  const copyText = async (text: string) => {
    let ok = false;
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
        ok = true;
      }
    } catch {
      ok = false;
    }
    if (!ok) {
      try {
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        ok = document.execCommand("copy");
        document.body.removeChild(ta);
      } catch {
        ok = false;
      }
    }
    setFeedback(ok ? "已复制到剪贴板" : "复制失败");
  };

  /** Fetch + open the commit detail modal. */
  const viewCommitDetail = async (c: GitLogEntry) => {
    setCommitMenu(null);
    setCommitDetail("loading");
    try {
      const d = await invoke<GitCommitDetail>("git_show_commit", { repo: root, hash: c.hash });
      setCommitDetail(d);
    } catch (e) {
      setCommitDetail(null);
      setFeedback(String(e));
    }
  };

  /** Detach HEAD at a historical commit (with a confirm — moves the working tree). */
  const checkoutCommit = async (c: GitLogEntry) => {
    setCommitMenu(null);
    const short = c.hash.slice(0, 7);
    const ok = await confirm(
      `检出到提交 ${short}（游离 HEAD）？\n\n未提交的更改会阻止切换；之后可从分支菜单切回原分支。`,
      { title: "检出提交", kind: "warning" }
    );
    if (!ok) return;
    await runAction(
      () => invoke("git_checkout_commit", { repo: root, hash: c.hash }),
      `已检出 ${short}（游离 HEAD）`
    );
  };

  /** Create a branch: from a commit (stay put) or from the chip (create + switch). */
  const submitBranch = async () => {
    const name = branchName.trim();
    if (!name) return;
    setBranchErr("");
    const startAt = branchPrompt?.startAt;
    try {
      if (startAt) {
        await invoke("git_create_branch", { repo: root, name, startAt });
        setFeedback(`已创建分支 ${name}`);
      } else {
        await invoke("git_switch_new", { repo: root, name });
        setFeedback(`已创建并切换到分支 ${name}`);
      }
    } catch (e) {
      setBranchErr(String(e));
      return;
    }
    setBranchPrompt(null);
    setBranchName("");
    setCommitMenu(null);
    setBranchMenuOpen(false);
    await refresh();
  };

  /** Force-delete a branch (confirm first; the current branch is never offered). */
  const deleteBranch = async (name: string) => {
    const ok = await confirm(
      `强制删除分支「${name}」？未合并的提交会一并删除（仍可从 reflog 找回）。`,
      { title: "删除分支", kind: "warning" }
    );
    if (!ok) return;
    setBranchMenuOpen(false);
    await runAction(() => invoke("git_delete_branch", { repo: root, name }), `已删除分支 ${name}`);
  };

  const openInEditor = (path: string) => {
    const abs = root.endsWith("/") ? `${root}${path}` : `${root}/${path}`;
    const name = path.split("/").pop() || path;
    addTab(fileTab(abs, name));
    setActiveTab(`file:${abs}`);
  };

  /** Open the file's currently-loaded inline diff as a full editor diff tab
   *  (VS Code: clicking a change opens the side-by-side diff in the editor). */
  const openDiffInEditor = (path: string) => {
    const c = diffContent[path];
    if (!c) return;
    const abs = root.endsWith("/") ? `${root}${path}` : `${root}/${path}`;
    const name = path.split("/").pop() || path;
    addTab({
      id: `diff:${abs}`,
      type: "diff",
      filePath: abs,
      diffOld: c.old,
      diffNew: c.new,
      title: `${name} · diff`,
    });
    setActiveTab(`diff:${abs}`);
  };

  /** Discard a single file's unstaged changes (tracked → restore, untracked →
   *  delete). Confirmed first — destructive and not undoable. */
  const discardFile = async (e: GitEntry) => {
    const ok = await confirm(`放弃「${e.path}」的更改？此操作不可撤销。`, {
      title: "放弃更改",
      kind: "warning",
    });
    if (!ok) return;
    await runAction(async () => {
      await invoke("git_discard", { repo: root, files: [e.path] });
      setDiffFor(null);
    }, "已放弃更改");
  };

  /** Discard all unstaged changes (`git restore .`). Staged changes stay. */
  const discardAll = async () => {
    const ok = await confirm("放弃所有未暂存的更改？已暂存的更改不受影响。", {
      title: "放弃全部更改",
      kind: "warning",
    });
    if (!ok) return;
    await runAction(() => invoke("git_discard_all", { repo: root }), "已放弃全部未暂存更改");
  };

  // Ensure the current branch always appears in the branch menu even when the
  // branch list is stale/empty (e.g. non-git dir or a fresh checkout).
  const branchList = branches.some((b) => b.name === branch)
    ? branches
    : branch
      ? [{ name: branch, current: true }, ...branches]
      : branches;

  const isRepo = !!repoInfo?.is_repo;
  // A repo with no `remote` configured: Pull/Push would fail, so hint at it but
  // keep the rest of the panel functional.
  const noRemote = isRepo && !repoInfo?.has_remote;
  // "未推送" indicator: N commits ahead of upstream, or a branch that has never
  // been pushed (needs publishing — click to push / set upstream).
  const unpushedAhead = repoInfo?.ahead ?? 0;
  const needsPublish =
    isRepo && !!repoInfo?.has_remote && !repoInfo?.has_upstream && !!repoInfo?.branch;
  // Whether unpushed commits exist — drives the adaptive "推送" button state.
  const statusHintClickable = needsPublish || unpushedAhead > 0;
  // Standalone commit button. With unstaged changes but nothing staged it turns
  // into "+暂存全部" (one click stages everything) so the user can get to a
  // commit in a single step; otherwise it stays "提交".
  const noStagedWithChanges = staged.length === 0 && changes.length > 0;
  // Adaptive commit-area button, no hint bar — the label says the next step:
  //   unpushed commits → "推送"   (push)
  //   nothing staged, changes exist → "+暂存全部"  (stage all)
  //   otherwise → "提交"          (commit)
  const canPush = statusHintClickable;
  const commitButtonLabel = canPush ? "推送" : noStagedWithChanges ? "+暂存全部" : "提交";
  const commitButtonEnabled = canPush
    ? !busy
    : noStagedWithChanges
      ? !busy
      : !!msg.trim() && staged.length > 0 && !busy;
  const commitButtonTitle = canPush
    ? "推送本地提交到远程"
    : noStagedWithChanges
      ? "暂存全部更改"
      : "提交已暂存的更改";
  const onCommitClick = () => {
    if (canPush) return push();
    if (noStagedWithChanges) return stageAll();
    return commit();
  };

  const renderFileRow = (e: GitEntry, kind: "staged" | "changes") => {
    const code = kind === "staged" ? e.index : e.worktree;
    const { glyph, cls } = glyphFor(code);
    const open = diffFor === e.path;
    const isStagedRow = kind === "staged";
    return (
      <div key={e.path}>
        <div className={`gv-file ${cls}`} onClick={() => toggleDiff(e)}>
          <span className="gv-file-glyph">{glyph}</span>
          <span className="gv-file-path" title={e.path}>
            {e.path}
          </span>
          <span className="gv-file-actions">
            <span
              className="gv-file-act"
              title={isStagedRow ? "取消暂存" : "暂存"}
              onClick={(ev) => {
                ev.stopPropagation();
                toggleFile(e, kind);
              }}
            >
              {isStagedRow ? "−" : "+"}
            </span>
            {!isStagedRow && (
              <span
                className="gv-file-discard"
                title="放弃更改"
                onClick={(ev) => {
                  ev.stopPropagation();
                  void discardFile(e);
                }}
              >
                <Icon name="x" size={14} />
              </span>
            )}
            <span
              className="gv-file-diff"
              title="打开 diff"
              onClick={(ev) => {
                ev.stopPropagation();
                toggleDiff(e);
              }}
            >
              打开diff
            </span>
          </span>
        </div>
        {open && (
          <div className="gv-diff">
            <div className="gv-diff-head">
              <span className="gv-diff-path" title={e.path}>
                {e.path}
              </span>
              <span className="gv-diff-head-actions">
                <span
                  className="gv-diff-open"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    openDiffInEditor(e.path);
                  }}
                  title="在编辑器打开 diff 视图"
                >
                  打开
                </span>
                <span
                  className="gv-diff-open"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    openInEditor(e.path);
                  }}
                  title="打开源文件"
                >
                  编辑
                </span>
              </span>
            </div>
            {diffContent[e.path] ? (
              <InlineMergeDiff
                oldText={diffContent[e.path].old}
                newText={diffContent[e.path].new}
              />
            ) : (
              <div className="gv-diff-loading">加载 diff…</div>
            )}
          </div>
        )}
      </div>
    );
  };

  const renderGroup = (
    title: string,
    list: GitEntry[],
    actionLabel: string,
    onAction: () => void,
    open: boolean,
    onToggle: (v: boolean) => void,
    kind: "staged" | "changes"
  ) => (
    <div className="gv-group">
      <div className="gv-group-header" onClick={() => onToggle(!open)}>
        <span className="gv-arrow">{open ? <Icon name="chevron-down" size={12} /> : <Icon name="chevron-right" size={12} />}</span>
        <span className="gv-group-title">
          {title} ({list.length})
        </span>
        {list.length > 0 && (
          <span
            className="gv-group-action"
            onClick={(ev) => {
              ev.stopPropagation();
              onAction();
            }}
          >
            {actionLabel}
          </span>
        )}
      </div>
      {open &&
        (list.length === 0 ? (
          <div className="gv-empty">(无)</div>
        ) : (
          list.map((e) => renderFileRow(e, kind))
        ))}
    </div>
  );

  return (
    <div className="tab-panel gv-panel" id="tab-git" style={{ padding: 12 }}>
      {repoInfo && !repoInfo.is_repo ? (
        <div className="gv-scroll">
          <div className="up-git-init">
            <div className="up-git-init-text">该项目未初始化 git</div>
            <span
              className={`act-btn up-git-init-btn${busy ? " active" : ""}`}
              onClick={initRepo}
            >
              {busy ? "初始化中…" : "git init"}
            </span>
          </div>
        </div>
      ) : (
        <>
          <div className="gv-scroll">
          {/* Header row: branch + indicators left, refresh/more right */}
          <div className="gv-head">
            <span className="gv-branch-wrap">
              <span
                className={`gv-branch${branchMenuOpen ? " open" : ""}`}
                title={branch ? `当前分支: ${branch}（点击管理分支）` : "无分支（点击管理分支）"}
                onClick={() => {
                  setBranchMenuOpen((o) => !o);
                  setMenuOpen(false);
                  setCommitMenu(null);
                  setCommitDetail(null);
                }}
              >
                <Icon name="git-branch" size={12} /> {branch || "无分支"}
              </span>
              {branchMenuOpen && (
                <div
                  className="gv-branch-pop"
                  onClick={(e) => e.stopPropagation()}
                  onContextMenu={(e) => e.stopPropagation()}
                >
                  <div className="gv-menu-label">分支管理</div>
                  {branchPrompt == null ? (
                    <div
                      className="gv-branch-new"
                      onClick={() => {
                        setBranchPrompt({});
                        setBranchName("");
                        setBranchErr("");
                      }}
                    >
                      <Icon name="plus" size={12} /> 新建分支…
                    </div>
                  ) : (
                    <div className="gv-branch-new-input">
                      <input
                        autoFocus
                        value={branchName}
                        placeholder="新分支名…"
                        onChange={(e) => {
                          setBranchName(e.target.value);
                          setBranchErr("");
                        }}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            e.preventDefault();
                            void submitBranch();
                          } else if (e.key === "Escape") setBranchPrompt(null);
                        }}
                      />
                      {branchErr && <div className="ctx-newbranch-err">{branchErr}</div>}
                    </div>
                  )}
                  <div className="gv-menu-label">切换分支</div>
                  <div className="gv-menu-branches">
                    {branchList.length === 0 && <div className="gv-empty">无分支</div>}
                    {branchList.map((b) => (
                      <div key={b.name} className="gv-menu-branch-row">
                        <div
                          className={`gv-menu-branch${b.current ? " current" : ""}`}
                          title={b.current ? `当前分支: ${b.name}` : `切换到 ${b.name}`}
                          onClick={() => {
                            setBranchMenuOpen(false);
                            void switchBranch(b.name);
                          }}
                        >
                          {b.current ? (
                            <Icon name="dot" size={12} style={{ marginRight: 4 }} />
                          ) : (
                            <Icon name="circle" size={12} style={{ marginRight: 4 }} />
                          )}
                          <span className="gv-menu-branch-name">{b.name}</span>
                        </div>
                        {!b.current && (
                          <span
                            className="gv-branch-del"
                            title="强制删除分支"
                            onClick={(e) => {
                              e.stopPropagation();
                              void deleteBranch(b.name);
                            }}
                          >
                            <Icon name="x" size={12} />
                          </span>
                        )}
                      </div>
                    ))}
                  </div>
                  <div className="gv-menu-sep" />
                  <div className="gv-menu-item" onClick={() => { setBranchMenuOpen(false); pull(); }}>
                    拉取 (pull)
                  </div>
                  <div className="gv-menu-item" onClick={() => { setBranchMenuOpen(false); push(); }}>
                    推送 (push)
                  </div>
                </div>
              )}
            </span>
            <span className="gv-icon" onClick={refresh} title="刷新">
              <Icon name="rotate-cw" size={14} />
            </span>
            <span
              className="gv-icon gv-more"
              onClick={() => {
                setMenuOpen(!menuOpen);
                setBranchMenuOpen(false);
              }}
              title="更多"
            >
              <Icon name="ellipsis" size={14} />
            </span>
            {menuOpen && <div className="gv-backdrop" onClick={() => setMenuOpen(false)} />}
            {menuOpen && (
              <div className="gv-menu" onClick={(e) => e.stopPropagation()}>
                <div className="gv-menu-item" onClick={commitAndPush}>
                  提交并推送
                </div>
                <div className="gv-menu-item" onClick={pull}>
                  拉取
                </div>
                <div className="gv-menu-item" onClick={push}>
                  推送
                </div>
                <div className="gv-menu-label">分支管理</div>
                <div className="gv-menu-branches">
                  {branchList.length === 0 && <div className="gv-empty">无分支</div>}
                  {branchList.map((b) => (
                    <div
                      key={b.name}
                      className={`gv-menu-branch${b.current ? " current" : ""}`}
                      onClick={() => {
                        switchBranch(b.name);
                        setMenuOpen(false);
                      }}
                    >
                      {b.current ? (
                        <Icon name="dot" size={12} style={{ marginRight: 4 }} />
                      ) : (
                        <Icon name="circle" size={12} style={{ marginRight: 4 }} />
                      )}
                      {b.name}
                    </div>
                  ))}
                </div>
                <div className="gv-menu-sep" />
                <div className="gv-menu-item" onClick={stageAll}>
                  全部暂存
                </div>
                <div className="gv-menu-item" onClick={unstageAll}>
                  全部取消暂存
                </div>
                <div className="gv-menu-item gv-menu-danger" onClick={discardAll}>
                  全部放弃更改
                </div>
              </div>
            )}
          </div>

          {/* Commit input + standalone adaptive button */}
          <div className="gv-commit">
            <textarea
              className="gv-commit-input"
              placeholder="Commit message…  (Ctrl+Enter 提交)"
              rows={2}
              value={msg}
              onChange={(e) => setMsg(e.target.value)}
              onKeyDown={(e) => {
                if ((e.metaKey || e.ctrlKey) && e.key === "Enter") onCommitClick();
              }}
            />
            <button
              className="gv-commit-btn"
              title={commitButtonTitle}
              disabled={!commitButtonEnabled}
              onClick={onCommitClick}
            >
              {commitButtonLabel}
            </button>
          </div>

          {noRemote && (
            <div className="up-git-hint" title="未配置远程仓库">
              <Icon name="circle-slash" size={12} /> 无远程仓库（Pull / Push 不可用）
            </div>
          )}

          {error && <div className="gv-msg gv-error">{error}</div>}
          {feedback && <div className="gv-msg gv-feedback">{feedback}</div>}

          {/* Staged / Changes groups */}
          {renderGroup(
            "暂存的更改",
            staged,
            "全部取消暂存",
            unstageAll,
            stagedOpen,
            setStagedOpen,
            "staged"
          )}
          {renderGroup(
            "更改",
            changes,
            "全部暂存",
            stageAll,
            changesOpen,
            setChangesOpen,
            "changes"
          )}

          {/* 「已提交的更改」组已按用户要求隐藏（git_committed 拉取一并移除，
              恢复时从 git 历史找回 committed 组 JSX 即可）。 */}

          </div>

          {/* Commit history (DevPlan §7.4B): top-anchored below the change groups
              — `.gv-scroll` above sizes to its content, and this group grows
              (flex: 1 1 auto) to fill down to the panel bottom, never past it.
              When collapsed, flexGrow drops to 0 so the header alone doesn't
              stretch a tall empty box. */}
          <div className="gv-group gg-log" style={{ flexGrow: logOpen ? 1 : 0, flexShrink: logOpen ? 1 : 0 }}>
            <div className="gv-group-header gg-log-head" onClick={() => setLogOpen(!logOpen)}>
              <span className="gv-arrow">{logOpen ? <Icon name="chevron-down" size={12} /> : <Icon name="chevron-right" size={12} />}</span>
              <span className="gv-group-title">提交历史</span>
            </div>
            {logOpen && (
              <div className="gg-log-body">
                {log.length === 0 ? (
                  <div className="gg-log-empty">暂无提交记录</div>
                ) : (
                  <CommitGraph log={log} currentBranch={branch} menuOpen={!!commitMenu} onCommitContextMenu={openCommitMenu} />
                )}
              </div>
            )}
          </div>
        </>
      )}

      {/* Commit-tree right-click menu */}
      {commitMenu && (
        <div
          className="ctx-menu gv-commit-menu"
          style={{ left: commitMenu.x, top: commitMenu.y }}
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
        >
          <div className="ctx-label">{commitMenu.commit.hash.slice(0, 7)}</div>
          <div
            className="ctx-item"
            onClick={() => {
              void copyText(commitMenu.commit.hash);
              setCommitMenu(null);
            }}
          >
            <Icon name="copy" size={13} /> 复制提交哈希
          </div>
          <div
            className="ctx-item"
            onClick={() => {
              void copyText(commitMenu.commit.subject);
              setCommitMenu(null);
            }}
          >
            <Icon name="clipboard" size={13} /> 复制提交信息
          </div>
          <div className="ctx-sep" />
          <div className="ctx-item" onClick={() => viewCommitDetail(commitMenu.commit)}>
            <Icon name="eye" size={13} /> 查看提交详情
          </div>
          <div className="ctx-item" onClick={() => checkoutCommit(commitMenu.commit)}>
            <Icon name="git-pull-request" size={13} /> 检出此提交（游离 HEAD）
          </div>
          <div className="ctx-sep" />
          {branchPrompt?.startAt === commitMenu.commit.hash ? (
            <div className="ctx-newbranch">
              <input
                autoFocus
                value={branchName}
                placeholder="新分支名…"
                onChange={(e) => {
                  setBranchName(e.target.value);
                  setBranchErr("");
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void submitBranch();
                  } else if (e.key === "Escape") setBranchPrompt(null);
                }}
              />
              {branchErr && <div className="ctx-newbranch-err">{branchErr}</div>}
            </div>
          ) : (
            <div
              className="ctx-item"
              onClick={() => {
                setBranchPrompt({ startAt: commitMenu.commit.hash });
                setBranchName("");
                setBranchErr("");
              }}
            >
              <Icon name="git-branch" size={13} /> 从此提交新建分支…
            </div>
          )}
        </div>
      )}

      {/* Commit detail modal */}
      {commitDetail && (
        <div className="gv-detail-backdrop" onClick={() => setCommitDetail(null)}>
          <div className="gv-detail" onClick={(e) => e.stopPropagation()}>
            {commitDetail === "loading" ? (
              <div className="gv-detail-loading">加载中…</div>
            ) : (
              <>
                <div className="gv-detail-head">
                  <span className="gv-detail-hash">{commitDetail.hash.slice(0, 7)}</span>
                  <span
                    className="gv-detail-icon"
                    title="复制完整哈希"
                    onClick={() => copyText(commitDetail.hash)}
                  >
                    <Icon name="copy" size={14} />
                  </span>
                  <span
                    className="gv-detail-icon gv-detail-close"
                    title="关闭"
                    onClick={() => setCommitDetail(null)}
                  >
                    <Icon name="x" size={14} />
                  </span>
                </div>
                <div className="gv-detail-subject">{commitDetail.subject}</div>
                <div className="gv-detail-meta">
                  {commitDetail.author}
                  {commitDetail.email ? ` <${commitDetail.email}>` : ""} ·{" "}
                  {new Date(commitDetail.ts * 1000).toLocaleString("zh-CN")}
                </div>
                {commitDetail.body && (
                  <pre className="gv-detail-body">{commitDetail.body}</pre>
                )}
                <div className="gv-detail-files">
                  {commitDetail.files.length === 0 ? (
                    <div className="gv-detail-files-empty">（无文件改动）</div>
                  ) : (
                    commitDetail.files.map((f) => (
                      <div key={f.path} className="gv-detail-file">
                        <span className="gv-detail-file-status">{f.status}</span>
                        <span className="gv-detail-file-path" title={f.path}>
                          {f.path}
                        </span>
                        <span className="gv-detail-file-num">
                          {f.add > 0 && <span className="gv-detail-add">+{f.add}</span>}
                          {f.del > 0 && <span className="gv-detail-del">−{f.del}</span>}
                        </span>
                      </div>
                    ))
                  )}
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
