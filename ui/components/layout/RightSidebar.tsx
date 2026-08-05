import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";

type RightTab = "overview" | "files" | "git";

export function RightSidebar() {
  const [activeTab, setActiveTab] = useState<RightTab>("overview");
  const [reportCollapsed, setReportCollapsed] = useState(false);
  const reports = useStore((s) => s.reports);
  const latest = reports[0];

  return (
    <>
      <div className="resize-handle" id="resize-right" />
      <div className="right-sidebar">
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
                {latest ? fmtTs(latest.ts) : "—"}
              </div>
              <div className="report-quote">
                {latest ? latest.summary : "Waiting for agent activity…"}
              </div>
              <div className="report-meta">
                Task: {latest ? `worker ${latest.worker}` : "—"}
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

function fmtTs(ms: number): string {
  const d = new Date(ms);
  return d.toLocaleTimeString();
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

function OverviewDashboard() {
  const esp = useStore((s) => s.espStatus);
  const workerInfos = useStore((s) => s.workerInfos);
  const smartReturn = useStore((s) => s.smartReturn);

  const busyCount = workerInfos.filter((w) => w.status === "busy").length;

  const toggleSmartReturn = async () => {
    const { setSmartReturn } = await import("../../state/orchestration");
    setSmartReturn(!smartReturn);
  };

  return (
    <div className="tab-panel" id="tab-overview">
      <CollapsibleSection
        title="🤖 Worker 编排"
        summary={`${workerInfos.length} workers · ${busyCount} busy · 智能返回${smartReturn ? " 开" : " 关"}`}
      >
        {workerInfos.length === 0 && (
          <div className="ov-row">
            <span className="ov-label">没有 worker</span>
            <span className="ov-value">—</span>
          </div>
        )}
        {workerInfos.map((w) => (
          <div className="ov-row" key={w.id}>
            <span className="ov-label" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
              {w.title || w.id.slice(0, 6)}
            </span>
            <span className="ov-value">
              {w.status === "busy" ? "🟠 busy" : w.status === "offline" ? "⚫ offline" : "🟢 idle"}
            </span>
          </div>
        ))}
        <div className="ov-divider" />
        <div
          className="ov-row"
          style={{ cursor: "pointer" }}
          onClick={toggleSmartReturn}
          title="智能返回分级开关"
        >
          <span className="ov-label">智能返回</span>
          <span className="ov-value">{smartReturn ? "✅ 开" : "⬜ 关"}</span>
        </div>
      </CollapsibleSection>

      {/* Runtime */}
      <CollapsibleSection title="📊 Runtime" summary="📊 Runtime 0/1M Tokens">
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
      <CollapsibleSection title="用量分析" summary="用量分析 主模型— · 子代理—">
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

      {/* Computer */}
      <CollapsibleSection title="💻 Computer Status" summary="💻 🟢 Online">
        <div className="ov-row">
          <span className="ov-label">状态</span>
          <span className="ov-value gr">🟢 Online</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">CPU</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Memory</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Disk</span>
          <span className="ov-value">—</span>
        </div>
      </CollapsibleSection>

      {/* ESP */}
      <CollapsibleSection
        title="🔌 ESP Device Status"
        summary={`🔌 ${esp.connected ? (esp.name ?? "ESP") : "Not connected"}${esp.battery_pct !== null ? ` · 🔋${esp.battery_pct}%` : ""}`}
      >
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

  return (
    <div className={`ov-section${collapsed ? " collapsed" : ""}`}>
      <div
        className="ov-section-header"
        onClick={() => setCollapsed(!collapsed)}
      >
        {title}
        <span className="ov-toggle">{collapsed ? "▲" : "▼"}</span>
      </div>
      <div className="ov-summary">{summary}</div>
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

function FilesPanel() {
  const root = useProjectRoot();
  const [dirs, setDirs] = useState<Map<string, FsEntry[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
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

  const renderEntries = (dir: string, depth: number): React.ReactNode => {
    const entries = (dirs.get(dir) || [])
      .filter((e) => !SKIP_DIRS.has(e.name))
      .sort((a, b) => (a.is_dir === b.is_dir ? a.name.localeCompare(b.name) : a.is_dir ? -1 : 1));
    return entries.map((e) => {
      const path = `${dir}/${e.name}`;
      if (e.is_dir) {
        return (
          <div key={path}>
            <div
              className="dir"
              style={{ paddingLeft: depth * 14 }}
              onClick={() => toggleDir(path)}
            >
              {expanded.has(path) ? "▾" : "▸"} 📁 {e.name}
            </div>
            {expanded.has(path) && renderEntries(path, depth + 1)}
          </div>
        );
      }
      return (
        <div
          key={path}
          className="file"
          style={{ paddingLeft: depth * 14 }}
          onClick={() => openFile(path, e.name)}
        >
          📄 {e.name}
        </div>
      );
    });
  };

  return (
    <div className="tab-panel" id="tab-files" style={{ padding: "8px 0" }}>
      <div style={{ padding: "0 12px 8px", fontSize: 11, color: "var(--muted)", fontFamily: "var(--mono)" }}>
        {root}
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
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<GitEntry[]>("git_status", { dir: root })
      .then((list) => {
        setEntries(list ?? []);
        setError(null);
      })
      .catch((e) => {
        setEntries([]);
        setError(String(e));
      });
  }, [root]);

  const projName = root.split("/").pop();

  return (
    <div className="tab-panel" id="tab-git" style={{ padding: 12 }}>
      <div className="git-title">Changes · {projName}</div>
      {error && (
        <div style={{ fontSize: 11, color: "var(--warn)", marginBottom: 8 }}>{error}</div>
      )}
      <div className="git-changes">
        {entries.length === 0 && !error && (
          <div style={{ fontSize: 12, color: "var(--muted)" }}>工作区干净 ✅</div>
        )}
        {entries.map((e) => {
          const { glyph, cls } = statusGlyph(e);
          return (
            <div className={cls} key={e.path}>
              {glyph} {e.path}
            </div>
          );
        })}
      </div>
      <div className="git-actions">
        <span className="act-btn">Stage All</span>
        <span className="act-btn">Commit</span>
        <span className="act-btn">Pull</span>
        <span className="act-btn">Push</span>
      </div>
    </div>
  );
}
