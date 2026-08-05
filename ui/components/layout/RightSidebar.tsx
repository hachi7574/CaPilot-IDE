import { useState } from "react";

type RightTab = "overview" | "files" | "git";

export function RightSidebar() {
  const [activeTab, setActiveTab] = useState<RightTab>("overview");
  const [reportCollapsed, setReportCollapsed] = useState(false);

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
              <div className="report-time">—</div>
              <div className="report-quote">Waiting for agent activity…</div>
              <div className="report-meta">
                Task: —<br />
                Status: Idle
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

/* ── Overview Dashboard ───────────────────────────────────────── */

function OverviewDashboard() {
  return (
    <div className="tab-panel" id="tab-overview">
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

      {/* Rate limits */}
      <CollapsibleSection title="剩余用量" summary="剩余用量 5h — · 周— · 月—">
        <div className="ov-bar-row">
          <div className="ov-bar-label">⏱ 5小时窗口</div>
          <div className="ov-bar">
            <div className="ov-bar-fill gr" style={{ width: "100%" }} />
          </div>
          <div className="ov-bar-stats">
            <span />
            <span>剩余 100%</span>
          </div>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar-label">📅 周额度</div>
          <div className="ov-bar">
            <div className="ov-bar-fill ye" style={{ width: "100%" }} />
          </div>
          <div className="ov-bar-stats">
            <span />
            <span>剩余 100%</span>
          </div>
        </div>
        <div className="ov-bar-row">
          <div className="ov-bar-label">🗓 月额度</div>
          <div className="ov-bar">
            <div className="ov-bar-fill gr" style={{ width: "100%" }} />
          </div>
          <div className="ov-bar-stats">
            <span />
            <span>剩余 100%</span>
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
          <span className="ov-label">GPU</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Disk</span>
          <span className="ov-value">—</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Network</span>
          <span className="ov-value">—</span>
        </div>
      </CollapsibleSection>

      {/* ESP */}
      <CollapsibleSection title="🔌 ESP Device Status" summary="🔌 —">
        <div className="ov-esp-status">
          <span className="ov-esp-dot" />
          <span className="ov-esp-name">Not connected</span>
        </div>
        <div className="ov-row">
          <span className="ov-label">Connection</span>
          <span className="ov-esp-conn">—</span>
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

function FilesPanel() {
  return (
    <div className="tab-panel" id="tab-files" style={{ padding: "8px 0" }}>
      <div style={{ padding: "0 12px 8px" }}>
        <input
          className="files-search"
          style={{
            width: "100%",
            background: "var(--bg3)",
            border: "1px solid var(--rule2)",
            color: "var(--ink)",
            fontFamily: "var(--mono)",
            fontSize: 12,
            padding: "8px 10px",
            outline: "none",
          }}
          placeholder="🔍 搜索文件…"
        />
      </div>
      <div className="files-tree">
        <div className="dir">▸ 📁 src-tauri</div>
        <div className="dir">▾ 📁 ui</div>
        <div style={{ paddingLeft: 16 }} className="dir">
          ▸ 📁 components
        </div>
        <div style={{ paddingLeft: 16 }} className="dir">
          ▸ 📁 state
        </div>
        <div style={{ paddingLeft: 16 }} className="file">
          📄 App.tsx
        </div>
        <div style={{ paddingLeft: 16 }} className="file">
          📄 main.tsx
        </div>
        <div className="dir">▸ 📁 capabilities</div>
        <div className="file file-new">📄 tauri.conf.json</div>
        <div className="file">📄 Cargo.toml</div>
        <div className="file file-web">🌐 index.html</div>
      </div>
    </div>
  );
}

/* ── Git Panel ────────────────────────────────────────────────── */

function GitPanel() {
  return (
    <div className="tab-panel" id="tab-git" style={{ padding: 12 }}>
      <div className="git-title">Changes · CaPilot</div>
      <div className="git-changes">
        <div className="gm">
          M src/agent_runtime/adapter.rs{" "}
          <span className="gs">+12 -3</span>
        </div>
        <div className="gm">
          M ui/components/Composer.tsx{" "}
          <span className="gs">+45 -8</span>
        </div>
        <div className="ga">
          A src/esp/ble.rs <span className="gs">+230</span>
        </div>
        <div className="gd">
          D Doc/old-notes.md <span className="gs">-156</span>
        </div>
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
