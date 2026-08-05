import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { MergeView } from "@codemirror/merge";
import { useStore } from "../../state/store";

type RightTab = "overview" | "files" | "git";

export function RightSidebar() {
  const [activeTab, setActiveTab] = useState<RightTab>("overview");
  const [reportCollapsed, setReportCollapsed] = useState(false);
  // Re-render periodically so the report's relative timestamp stays fresh.
  const [, setTick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 10000);
    return () => clearInterval(t);
  }, []);
  const reports = useStore((s) => s.reports);
  const latest = reports[0];
  const rightWidth = useStore((s) => s.rightWidth);
  const setRightWidth = useStore((s) => s.setRightWidth);

  // Draggable right sidebar resize (drag right → narrower).
  const startRightResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = rightWidth;
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(520, Math.max(260, startWidth - (ev.clientX - startX)));
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
      <div className="resize-handle" id="resize-right" onMouseDown={startRightResize} />
      <div className="right-sidebar" style={{ width: rightWidth }}>
        {/* Tabs */}
        <div className="right-tabs">
          <div
            className={`right-tab${activeTab === "overview" ? " active" : ""}`}
            onClick={() => setActiveTab("overview")}
          >
            ∿ 概览
          </div>
          <div
            className={`right-tab${activeTab === "files" ? " active" : ""}`}
            onClick={() => setActiveTab("files")}
          >
            📄 文件
          </div>
          <div
            className={`right-tab${activeTab === "git" ? " active" : ""}`}
            onClick={() => setActiveTab("git")}
          >
            ⚒ Git
          </div>
        </div>

        {/* Tab Content */}
        <div className="right-panel">
          {activeTab === "overview" && <OverviewDashboard />}
          {activeTab === "files" && <FilesPanel />}
          {activeTab === "git" && <GitPanel />}
        </div>

        {/* Master Report (always visible) */}
        <div className="master-report">
          <div
            className="report-header"
            onClick={() => setReportCollapsed(!reportCollapsed)}
          >
            🤖 Master Agent Report
            <span className="report-toggle">{reportCollapsed ? "▲" : "▼"}</span>
          </div>
          {!reportCollapsed && (
            <div className="report-body">
              <div className="report-time">
                {latest ? fmtRelTime(latest.ts) : "—"}
              </div>
              <div className="report-quote">
                {latest ? `"${latest.summary}"` : "Waiting for agent activity…"}
              </div>
              <div className="report-meta">
                Task: {latest ? latest.worker : "—"}
                <br />
                Status: {latest ? latest.level : "Idle"}
              </div>
              <div className="report-actions">
                <span className="rbtn">展开任务详情</span>
                <span className="rbtn">跳转到 Master 终端</span>
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/** Relative timestamp, e.g. "20s ago" / "3m ago" (matches the preview). */
function fmtRelTime(ms: number): string {
  const diff = Math.max(0, Date.now() - ms);
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
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

/* ── Overview Dashboard ───────────────────────────────────────── */

/** System-wide CPU/MEM snapshot from the backend `system_stats` command. */
interface SystemStats {
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
}

/** CPU% to 2 decimals, e.g. "23.12%". */
function fmtCpu(cpu: number | undefined | null): string {
  if (cpu === undefined || cpu === null || Number.isNaN(cpu)) return "—";
  return `${cpu.toFixed(2)}%`;
}

/** Bytes → GB with 2 decimals, e.g. "8.32G". */
function bytesToG(bytes: number | undefined | null): string {
  if (bytes === undefined || bytes === null || Number.isNaN(bytes)) return "—";
  return `${(bytes / 1024 ** 3).toFixed(2)}G`;
}

function OverviewDashboard() {
  const esp = useStore((s) => s.espStatus);

  // Live system-wide CPU/MEM for the Computer Status panel (2s tick).
  const [stats, setStats] = useState<SystemStats | null>(null);
  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      invoke<SystemStats>("system_stats")
        .then((s) => {
          if (!cancelled) setStats(s);
        })
        .catch(() => {
          if (!cancelled) setStats(null);
        });
    };
    tick();
    const timer = setInterval(tick, 2000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  const cpuPct = stats?.cpu_pct;
  const memText =
    stats && stats.mem_used != null && stats.mem_total
      ? `${bytesToG(stats.mem_used)} / ${bytesToG(stats.mem_total)}`
      : "—";
  const cpuWidth = cpuPct != null ? Math.max(0, Math.min(100, cpuPct)) : 0;
  const memWidth =
    stats && stats.mem_total
      ? Math.max(0, Math.min(100, (stats.mem_used / stats.mem_total) * 100))
      : 0;

  const espKind =
    esp.kind === "ble" ? "🔵BLE" : esp.kind === "wifi" ? "📶WiFi" : esp.kind === "usb" ? "🔌USB" : "";
  const espSummary = `🔌 ESP 设备状态 ${esp.connected ? (esp.name ?? "ESP") : "未连接"}${espKind ? ` · ${espKind}` : ""}${esp.battery_pct != null ? ` · 🔋${esp.battery_pct}%` : ""}`;

  return (
    <div className="tab-panel" id="tab-overview">
      {/* Runtime */}
      <CollapsibleSection title="📊 运行时" summary="📊 运行时 0/1M · 缓存—">
        <div className="ov-row">
          <span className="ov-label">上下文窗口</span>
          <span className="ov-value gr">🟢 充足</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">已用 / 上限</span>
          <span className="ov-value">0 / 1,000,000</span>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar">
            <div className="ov-bar-fill pu" style={{ width: "0%" }} />
          </div>
          <div className="ov-bar-stats">
            <span>已用 0</span>
            <span>距压缩 1M</span>
          </div>
        </div>
        <div className="ov-divider" />
        <div className="ov-row">
          <span className="ov-label">缓存命中率</span>
          <span className="ov-value pu">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">运行时间</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">请求次数</span>
          <span className="ov-value">0</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">累计 Tokens</span>
          <span className="ov-value">0</span>
        </div>
      </CollapsibleSection>

      {/* Usage */}
      <CollapsibleSection title="📈 用量分析" summary="📈 用量分析 主模型— · 子代理—">
        <div className="ov-usage-item">
          <div className="ov-usage-label">主模型</div>
          <div className="ov-bar">
            <div className="ov-bar-fill pu" style={{ width: "0%" }} />
          </div>
          <div className="ov-usage-stats">
            <span>0 Tokens</span>
            <span>0%</span>
            <span>Cache —</span>
          </div>
        </div>
        <div className="ov-usage-item">
          <div className="ov-usage-label">子代理</div>
          <div className="ov-bar">
            <div className="ov-bar-fill ai" style={{ width: "0%" }} />
          </div>
          <div className="ov-usage-stats">
            <span>0 Tokens</span>
            <span>0%</span>
            <span>Cache —</span>
          </div>
        </div>
      </CollapsibleSection>

      {/* Remaining quota */}
      <CollapsibleSection title="⏱ 剩余用量" summary="⏱ 剩余用量 5h — · 周— · 月—">
        <div className="ov-bar-row">
          <div className="ov-bar-label">⏱ 5小时窗口</div>
          <div className="ov-bar">
            <div className="ov-bar-fill gr" style={{ width: "0%" }} />
          </div>
          <div className="ov-bar-stats">
            <span></span>
            <span>剩余 —</span>
          </div>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar-label">📅 周额度</div>
          <div className="ov-bar">
            <div className="ov-bar-fill ye" style={{ width: "0%" }} />
          </div>
          <div className="ov-bar-stats">
            <span></span>
            <span>剩余 —</span>
          </div>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar-label">🗓 月额度</div>
          <div className="ov-bar">
            <div className="ov-bar-fill gr" style={{ width: "0%" }} />
          </div>
          <div className="ov-bar-stats">
            <span></span>
            <span>剩余 —</span>
          </div>
        </div>
      </CollapsibleSection>

      {/* Computer */}
      <CollapsibleSection
        title="💻 电脑状态"
        summary={`💻 电脑状态 CPU ${fmtCpu(cpuPct)} · 内存 ${memText}`}
      >
        <div className="ov-bar-row">
          <div className="ov-row">
            <span className="ov-label">CPU</span>
            <span className="ov-value">{fmtCpu(cpuPct)}</span>
          </div>
          <div className="ov-bar">
            <div className="ov-bar-fill pu" style={{ width: `${cpuWidth}%` }} />
          </div>
        </div>
        <div className="ov-bar-row">
          <div className="ov-row">
            <span className="ov-label">内存</span>
            <span className="ov-value">{memText}</span>
          </div>
          <div className="ov-bar">
            <div className="ov-bar-fill gr" style={{ width: `${memWidth}%` }} />
          </div>
        </div>
      </CollapsibleSection>

      {/* ESP */}
      <CollapsibleSection title="🔌 ESP 设备状态" summary={espSummary}>
        <div className="ov-esp-status">
          <span
            className="ov-esp-dot"
            style={
              esp.connected
                ? { background: "var(--success)", borderColor: "var(--success)" }
                : { background: "var(--muted)", borderColor: "var(--muted)" }
            }
          />
          <span className="ov-esp-name">
            {esp.connected ? (esp.name ?? "ESP32-C5") : "Not connected"}
          </span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Connection</span>
          <span className="ov-esp-conn">
            {esp.connected
              ? esp.kind === "wifi"
                ? "📶 WiFi"
                : esp.kind === "usb"
                ? "🔌 USB"
                : "🔵 Bluetooth"
              : "—"}
          </span>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar-label">
            🔋 Battery {esp.battery_pct != null ? `${esp.battery_pct}%` : "—"}
          </div>
          <div className="ov-bar">
            <div
              className="ov-bar-fill gr"
              style={{ width: `${esp.battery_pct != null ? Math.max(0, Math.min(100, esp.battery_pct)) : 0}%` }}
            />
          </div>
        </div>
        <div className="ov-row">
          <span className="ov-label">Temperature</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Signal</span>
          <span className="ov-value">{esp.rssi != null ? `${esp.rssi}dBm` : "—"}</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Firmware</span>
          <span className="ov-value">—</span>
        </div>
      </CollapsibleSection>
    </div>
  );
}

/* ── Collapsible Section ──────────────────────────────────────── */

function CollapsibleSection({
  title,
  summary,
  children,
}: {
  title: string;
  summary: string;
  children: React.ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(false);

  // The summary strings include the title prefix (e.g. "📊 运行时 0/1M · 缓存—").
  // The header already shows the title, so strip the prefix and show only the
  // condensed VALUES inline when collapsed — the title bar keeps its exact
  // font/height/color in both states.
  const value = summary.startsWith(title)
    ? summary.slice(title.length).trimStart()
    : summary;

  return (
    <div className={`ov-section uk-section${collapsed ? " collapsed" : ""}`}>
      <div
        className="ov-section-header"
        onClick={() => setCollapsed(!collapsed)}
      >
        {title}
        {collapsed && value && (
          <span className="ov-summary-inline">{value}</span>
        )}
        <span className="ov-toggle">{collapsed ? "▲" : "▼"}</span>
      </div>
      {!collapsed && <div className="ov-section-body">{children}</div>}
    </div>
  );
}

/* ── Files Panel ──────────────────────────────────────────────── */

interface FsEntry {
  name: string;
  is_dir: boolean;
}

const SKIP_DIRS = new Set([".git", "node_modules", "target", ".claude", "dist", "build"]);

/** web/html files → warn (.file-web), config files → success (.rtx-file-conf). */
function fileClass(name: string): { cls: string; icon: string } {
  const isWeb = name === "index.html" || /\.html?$/.test(name);
  const isConf = name === "tauri.conf.json" || /\.(conf|config)\.json$/.test(name);
  if (isWeb) return { cls: "file file-web", icon: "🌐" };
  if (isConf) return { cls: "file rtx-file-conf", icon: "📄" };
  return { cls: "file", icon: "📄" };
}

function FilesPanel() {
  const root = useProjectRoot();
  const [dirs, setDirs] = useState<Map<string, FsEntry[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const addTab = useStore((s) => s.addTab);

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
    addTab({ id: `file:${path}`, type: "editor", filePath: path, title: name });
  };

  const isFiltering = filter.trim() !== "";
  const q = filter.trim().toLowerCase();

  const renderEntries = (dir: string, depth: number): React.ReactNode => {
    const entries = (dirs.get(dir) || [])
      .filter((e) => !SKIP_DIRS.has(e.name))
      // While filtering, keep every directory visible so matching files deep
      // down stay reachable; files are matched by name.
      .filter((e) => q === "" || e.is_dir || e.name.toLowerCase().includes(q))
      .sort((a, b) => (a.is_dir === b.is_dir ? a.name.localeCompare(b.name) : a.is_dir ? -1 : 1));
    return entries.map((e) => {
      const path = `${dir}/${e.name}`;
      if (e.is_dir) {
        const open = isFiltering || expanded.has(path);
        return (
          <div key={path}>
            <div
              className="dir"
              style={{ paddingLeft: depth * 14 }}
              onClick={() => toggleDir(path)}
              draggable
              onDragStart={(ev) => {
                ev.dataTransfer.setData("text/plain", path);
                ev.dataTransfer.effectAllowed = "copy";
              }}
              title={path}
            >
              {open ? "▾" : "▸"} 📁 {e.name}
            </div>
            {open && renderEntries(path, depth + 1)}
          </div>
        );
      }
      const { cls, icon } = fileClass(e.name);
      return (
        <div
          key={path}
          className={cls}
          style={{ paddingLeft: depth * 14 }}
          onClick={() => openFile(path, e.name)}
          title={path}
          draggable
          onDragStart={(ev) => {
            ev.dataTransfer.setData("text/plain", path);
            ev.dataTransfer.effectAllowed = "copy";
          }}
        >
          {icon} {e.name}
        </div>
      );
    });
  };

  return (
    <div className="tab-panel" id="tab-files" style={{ padding: "8px 0" }}>
      <div className="files-search" style={{ padding: "0 12px 8px" }}>
        <input
          type="text"
          placeholder="🔍 搜索文件…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      <div className="files-tree">{renderEntries(root, 0)}</div>
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

interface GitLogEntry {
  hash: string;
  subject: string;
  author: string;
  ts: number;
}

/** Rust `RepoInfo` from `git_repo_info` — whether the root is a git repo. */
interface RepoInfo {
  is_repo: boolean;
  has_remote: boolean;
  branch: string | null;
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

/** Unix-seconds timestamp → compact "MM-DD HH:mm" (zh-CN). */
function fmtTs(sec: number): string {
  if (!sec) return "—";
  const d = new Date(sec * 1000);
  return d.toLocaleString("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** OLD (left) / NEW (right) file content feeding the @codemirror/merge view. */
interface DiffContent {
  old: string;
  new: string;
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
      oneDark,
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
    try {
      const list = await invoke<GitEntry[]>("git_status", { dir: root });
      setEntries(list ?? []);
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
      const lg = await invoke<GitLogEntry[]>("git_log", { repo: root, count: 20 });
      setLog(lg ?? []);
    } catch {
      setLog([]);
    }
    // Inline diffs reference a specific file state; drop them so a stage/unstage
    // never shows a stale old/new pair after refresh.
    setDiffFor(null);
    setDiffContent({});
  };

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

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
      await invoke("git_commit", { repo: root, message: m });
      setMsg("");
    }, "提交成功");

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

  const openInEditor = (path: string) => {
    const abs = root.endsWith("/") ? `${root}${path}` : `${root}/${path}`;
    const name = path.split("/").pop() || path;
    addTab({ id: `file:${abs}`, type: "editor", filePath: abs, title: name });
    setActiveTab(`file:${abs}`);
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
              <span
                className="gv-diff-open"
                onClick={(ev) => {
                  ev.stopPropagation();
                  openInEditor(e.path);
                }}
              >
                打开
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
        <span className="gv-arrow">{open ? "▾" : "▸"}</span>
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
    <div className="tab-panel" id="tab-git" style={{ padding: 12 }}>
      {repoInfo && !repoInfo.is_repo ? (
        <div className="up-git-init">
          <div className="up-git-init-text">该项目未初始化 git</div>
          <span
            className={`act-btn up-git-init-btn${busy ? " active" : ""}`}
            onClick={initRepo}
          >
            {busy ? "初始化中…" : "git init"}
          </span>
        </div>
      ) : (
        <>
          {/* Header row: title + branch + refresh + more-menu */}
          <div className="gv-head">
            <span className="gv-title">源代码管理</span>
            <span className="gv-branch" title={branch ? `当前分支: ${branch}` : "无分支"}>
              ⎇ {branch || "无分支"}
            </span>
            <span className="gv-icon" onClick={refresh} title="刷新">
              ↻
            </span>
            <span className="gv-icon gv-more" onClick={() => setMenuOpen(!menuOpen)} title="更多">
              ⋯
            </span>
            {menuOpen && <div className="gv-backdrop" onClick={() => setMenuOpen(false)} />}
            {menuOpen && (
              <div className="gv-menu" onClick={(e) => e.stopPropagation()}>
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
                      {b.current ? "● " : "○ "}
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
              </div>
            )}
          </div>

          {/* Commit box: message textarea + check button */}
          <div className="gv-commit">
            <textarea
              className="gv-commit-input"
              placeholder="Commit message…"
              rows={2}
              value={msg}
              onChange={(e) => setMsg(e.target.value)}
              onKeyDown={(e) => {
                if ((e.metaKey || e.ctrlKey) && e.key === "Enter") commit();
              }}
            />
            <button
              className="gv-commit-btn"
              title="提交"
              disabled={!msg.trim() || busy}
              onClick={commit}
            >
              ✓
            </button>
          </div>

          {noRemote && (
            <div className="up-git-hint" title="未配置远程仓库">
              🚫 无远程仓库（Pull / Push 不可用）
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

          {entries.length === 0 && !error && <div className="gv-clean">工作区干净 ✅</div>}

          {/* Commit history (DevPlan §7.4B) */}
          <div className="gg-log">
            <div className="gg-log-head" onClick={() => setLogOpen(!logOpen)}>
              <span>提交历史</span>
              <span className="gg-log-toggle">{logOpen ? "▼" : "▶"}</span>
            </div>
            {logOpen && (
              <div className="gg-log-body">
                {log.length === 0 && <div className="gg-log-empty">暂无提交记录</div>}
                {log.map((c) => (
                  <div
                    key={c.hash}
                    className="gg-log-row"
                    title={`${c.author} · ${fmtTs(c.ts)}`}
                  >
                    <span className="gg-log-hash">{c.hash}</span>
                    <span className="gg-log-subject">{c.subject}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
