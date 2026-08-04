import { useState } from "react";

type RightTab = "overview" | "files" | "git";

export function RightSidebar() {
  const [activeTab, setActiveTab] = useState<RightTab>("overview");

  return (
    <div className="right-sidebar">
      {/* Tabs */}
      <div className="right-sidebar-tabs">
        <div
          className={`right-tab ${activeTab === "overview" ? "active" : ""}`}
          onClick={() => setActiveTab("overview")}
        >
          ∿ Overview
        </div>
        <div
          className={`right-tab ${activeTab === "files" ? "active" : ""}`}
          onClick={() => setActiveTab("files")}
        >
          📄 Files
        </div>
        <div
          className={`right-tab ${activeTab === "git" ? "active" : ""}`}
          onClick={() => setActiveTab("git")}
        >
          ⚒ Git
        </div>
      </div>

      {/* Panel content */}
      <div className="right-panel">
        {activeTab === "overview" && <OverviewPanel />}
        {activeTab === "files" && (
          <div style={{ color: "var(--muted)", fontSize: 12 }}>
            File tree coming soon
          </div>
        )}
        {activeTab === "git" && (
          <div style={{ color: "var(--muted)", fontSize: 12 }}>
            Git panel coming soon
          </div>
        )}
      </div>

      {/* Master Report (always visible at bottom) */}
      <div
        style={{
          borderTop: "1px solid var(--rule)",
          padding: "12px",
          fontSize: 11,
          color: "var(--muted)",
        }}
      >
        <div style={{ fontFamily: "var(--mono)", marginBottom: 4 }}>
          🤖 Master Report
        </div>
        <div style={{ color: "var(--ink2)" }}>No reports yet</div>
      </div>
    </div>
  );
}

function OverviewPanel() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {/* Runtime */}
      <Section title="📊 Runtime">
        <MetricRow label="Context" value="0 / 1,000,000" />
        <MetricRow label="Used" value="0 tokens" />
      </Section>

      {/* Session */}
      <Section title="Session">
        <MetricRow label="Requests" value="0" />
        <MetricRow label="Duration" value="—" />
      </Section>

      {/* Computer */}
      <Section title="💻 Computer">
        <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
          <span
            className="status-dot running"
            style={{ display: "inline-block" }}
          />
          <span style={{ fontSize: 11, color: "var(--ink2)" }}>Online</span>
        </div>
      </Section>
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div
        style={{
          fontFamily: "var(--mono)",
          fontSize: 10,
          color: "var(--muted)",
          letterSpacing: 1,
          textTransform: "uppercase",
          marginBottom: 8,
          paddingBottom: 4,
          borderBottom: "1px solid var(--rule)",
        }}
      >
        {title}
      </div>
      {children}
    </div>
  );
}

function MetricRow({ label, value }: { label: string; value: string }) {
  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        fontSize: 11,
        padding: "2px 0",
      }}
    >
      <span style={{ color: "var(--muted)" }}>{label}</span>
      <span style={{ color: "var(--ink2)", fontFamily: "var(--mono)" }}>
        {value}
      </span>
    </div>
  );
}
