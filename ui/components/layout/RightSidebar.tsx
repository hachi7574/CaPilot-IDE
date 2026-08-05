import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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

/** Root directory for the file tree / git panel (active agent's cwd). */
function useProjectRoot(): string {
  const agents = useStore((s) => s.agents);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const cwd = activeTab?.agentId ? agents.get(activeTab.agentId)?.cwd : undefined;
  const [fallback, setFallback] = useState("/tmp");
  useEffect(() => {
    invoke<string>("workspace_root")
      .then(setFallback)
      .catch(() => {});
  }, []);
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

  if (collapsed) {
    return (
      <div className="ov-section uk-section collapsed">
        <div className="ov-summary uf-ov-summary uk-summary" onClick={() => setCollapsed(false)}>
          {summary}
        </div>
      </div>
    );
  }

  return (
    <div className="ov-section uk-section">
      <div className="ov-section-header" onClick={() => setCollapsed(true)}>
        {title}
        <span className="ov-toggle">▼</span>
      </div>
      <div className="ov-section-body">{children}</div>
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

function statusGlyph(e: GitEntry): { glyph: string; cls: string } {
  const code = (e.index + e.worktree).trim();
  if (code === "??") return { glyph: "A", cls: "ga" };
  if (code === "M" || code === "MM" || code === " M" || code === "M ") return { glyph: "M", cls: "gm" };
  if (e.index === "A") return { glyph: "A", cls: "ga" };
  if (e.index === "D" || e.worktree === "D") return { glyph: "D", cls: "gd" };
  if (e.index === "R") return { glyph: "R", cls: "gr" };
  return { glyph: code || "·", cls: "gm" };
}

function GitPanel() {
  const root = useProjectRoot();
  const [entries, setEntries] = useState<GitEntry[]>([]);
  const [branch, setBranch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [diffFor, setDiffFor] = useState<string | null>(null);
  const [diffText, setDiffText] = useState<Record<string, string>>({});

  const refresh = async () => {
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

  const stageAll = () =>
    runAction(async () => {
      const files = entries.map((e) => e.path);
      if (files.length === 0) return;
      await invoke("git_stage", { repo: root, files });
    }, "已暂存全部变更");

  const toggleFile = (e: GitEntry) => {
    const staged = e.index !== " " && e.index !== "?";
    return runAction(async () => {
      await invoke(staged ? "git_unstage" : "git_stage", {
        repo: root,
        files: [e.path],
      });
      setDiffFor(null);
    }, staged ? "已取消暂存" : "已暂存");
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

  const toggleDiff = async (e: GitEntry) => {
    if (diffFor === e.path) {
      setDiffFor(null);
      return;
    }
    setDiffFor(e.path);
    try {
      const text = await invoke<string>("git_diff", { repo: root, file: e.path });
      setDiffText((prev) => ({ ...prev, [e.path]: text }));
    } catch {
      setDiffText((prev) => ({ ...prev, [e.path]: "" }));
    }
  };

  const projName = root.split("/").pop();

  return (
    <div className="tab-panel" id="tab-git" style={{ padding: 12 }}>
      <div className="git-title" title={branch ? `⎇ ${branch}` : undefined}>
        Changes · {projName}
      </div>
      {error && (
        <div style={{ fontSize: 11, color: "var(--warn)", marginBottom: 8 }}>{error}</div>
      )}
      {feedback && (
        <div style={{ fontSize: 11, color: "var(--muted)", marginBottom: 8 }}>{feedback}</div>
      )}
      <div className="git-changes">
        {entries.length === 0 && !error && (
          <div style={{ fontSize: 12, color: "var(--muted)" }}>工作区干净 ✅</div>
        )}
        {entries.map((e) => {
          const { glyph, cls } = statusGlyph(e);
          const staged = e.index !== " " && e.index !== "?";
          const counts =
            e.add + e.del > 0 ? `+${e.add} -${e.del}` : staged ? "staged" : "";
          return (
            <div key={e.path}>
              <div className={`${cls} git-row`} onClick={() => toggleFile(e)}>
                <span
                  className="git-toggle"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    toggleDiff(e);
                  }}
                  title="查看 diff"
                >
                  {diffFor === e.path ? "▾" : "▸"}
                </span>
                <span className="git-glyph">{glyph}</span>
                <span className="git-path">{e.path}</span>
                <span className="git-counts">{counts}</span>
              </div>
              {diffFor === e.path && (
                <pre className="git-diff">
                  {diffText[e.path] || "(无 diff — 可能为已暂存或二进制变更)"}
                </pre>
              )}
            </div>
          );
        })}
      </div>
      <div className="git-commit" style={{ marginTop: 12 }}>
        <input
          type="text"
          placeholder="Commit message…"
          value={msg}
          onChange={(e) => setMsg(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && commit()}
          style={{
            width: "100%",
            background: "var(--bg3)",
            border: "1px solid var(--rule2)",
            color: "var(--ink)",
            fontFamily: "var(--mono)",
            fontSize: 12,
            padding: "6px 8px",
            outline: "none",
          }}
        />
      </div>
      <div className="git-actions">
        <span className={`act-btn${busy ? " active" : ""}`} onClick={stageAll}>
          Stage All
        </span>
        <span className="act-btn" onClick={commit}>
          Commit
        </span>
        <span className="act-btn" onClick={pull}>
          Pull
        </span>
        <span className="act-btn" onClick={push}>
          Push
        </span>
        <span className="act-btn" onClick={refresh} title="刷新">
          ↻
        </span>
      </div>
    </div>
  );
}
