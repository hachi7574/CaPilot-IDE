import { useEffect, useState } from "react";
import { useStore, ResourcePoint } from "../../state/store";
import { connectEsp, disconnectEsp } from "../../state/esp";
import { setSmartReturn } from "../../state/orchestration";
import { fetchResourceHistory, fmtCpu, fmtMem } from "../../state/resource";

export function StatusBar() {
  const agents = useStore((s) => s.agents);
  const workerInfos = useStore((s) => s.workerInfos);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const smartReturn = useStore((s) => s.smartReturn);
  const espStatus = useStore((s) => s.espStatus);
  const espConnecting = useStore((s) => s.espConnecting);
  const agentResources = useStore((s) => s.agentResources);
  const resourceHistory = useStore((s) => s.resourceHistory);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const [resourceOpen, setResourceOpen] = useState(false);

  const workerCount = Math.max(
    workerInfos.length,
    [...agents.values()].filter((a) => a.role === "worker").length
  );

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeAgentId = activeTab?.agentId ?? null;
  const activeRes = activeAgentId ? agentResources.get(activeAgentId) : undefined;

  const handleEspClick = async () => {
    if (espStatus.connected) {
      await disconnectEsp();
    } else {
      const err = await connectEsp();
      if (err) console.error("ESP connect failed:", err);
    }
  };

  const toggleSmartReturn = () => setSmartReturn(!smartReturn);

  return (
    <div className="statusbar">
      <span
        className="sb-item"
        onClick={handleEspClick}
        style={{ cursor: "pointer" }}
        title={espStatus.connected ? "ESP connected — click to disconnect" : "Click to connect ESP (BLE)"}
      >
        <span className={`sb-dot ${espStatus.connected ? "bt" : ""}`} />
        {espStatus.connected
          ? `${espStatus.kind === "wifi" ? "📶 WiFi" : espStatus.kind === "usb" ? "🔌 USB" : "🔵 BLE"}`
          : espConnecting
          ? "connecting…"
          : "📡 ESP off"}
      </span>
      <span className="sb-item">📶 WiFi</span>
      <span className="sb-sep" />
      <span className={`sb-item sb-battery`}>
        🔋 {espStatus.battery_pct !== null ? `${espStatus.battery_pct}%` : "—"}
      </span>
      <span className="sb-sep" />
      <span
        className={`sb-item${smartReturn ? "" : " off"}`}
        onClick={toggleSmartReturn}
        style={{ cursor: "pointer" }}
        title="master 智能返回（分级汇报）开关"
      >
        汇报🔔 {smartReturn ? "开" : "关"}
      </span>
      <span className="sb-item">worker×{workerCount}</span>
      <span className="sb-sep" />
      <span
        className="sb-item resource-item"
        onClick={() => setResourceOpen((o) => !o)}
        style={{ cursor: "pointer", position: "relative" }}
        title="资源监视（点击查看全部 agent 曲线）"
      >
        ⚙ CPU {fmtCpu(activeRes?.cpu_pct)} MEM {fmtMem(activeRes?.mem_bytes)}
        {resourceOpen && (
          <ResourcePopover
            agents={agents}
            agentResources={agentResources}
            resourceHistory={resourceHistory}
            activeAgentId={activeAgentId}
            onClose={() => setResourceOpen(false)}
          />
        )}
      </span>
      <span className="sb-spacer" />
      <span className="sb-item sb-mode">模式 {permissionMode}</span>
      <span className="sb-item">速度 {speed}</span>
    </div>
  );
}

/* ── Resource popover (DevPlan §10) ──────────────────────────── */

function ResourcePopover({
  agents,
  agentResources,
  resourceHistory,
  activeAgentId,
  onClose,
}: {
  agents: Map<string, { id: string; title: string }>;
  agentResources: Map<string, ResourcePoint>;
  resourceHistory: Map<string, ResourcePoint[]>;
  activeAgentId: string | null;
  onClose: () => void;
}) {
  // Backfill the curve from the backend-buffered history when opened.
  useEffect(() => {
    if (!activeAgentId) return;
    fetchResourceHistory(activeAgentId).then((points) => {
      if (!points.length) return;
      const s = useStore.getState();
      const cur = s.resourceHistory.get(activeAgentId) || [];
      if (points.length >= cur.length) {
        s.setResourceHistory(activeAgentId, points);
      }
    });
  }, [activeAgentId]);

  const rows = [...agents.values()];

  return (
    <div className="resource-popover" onMouseLeave={onClose}>
      <div className="resource-popover-title">⚙ 资源监视</div>
      <div className="resource-list">
        {rows.length === 0 && (
          <div className="resource-empty">没有运行中的 agent</div>
        )}
        {rows.map((a) => {
          const r = agentResources.get(a.id);
          return (
            <div className="resource-row" key={a.id}>
              <span className="resource-name" title={a.id}>
                {a.title || a.id.slice(0, 6)}
              </span>
              <span className="resource-val">CPU {fmtCpu(r?.cpu_pct)}</span>
              <span className="resource-val">MEM {fmtMem(r?.mem_bytes)}</span>
            </div>
          );
        })}
      </div>
      {activeAgentId && (
        <div className="resource-curve">
          <div className="resource-curve-label">
            曲线 · {activeAgentId.slice(0, 6)} · CPU
          </div>
          <Sparkline points={resourceHistory.get(activeAgentId) || []} />
        </div>
      )}
    </div>
  );
}

/** Inline-SVG sparkline of the last N CPU samples (no chart library). */
function Sparkline({ points, width = 220, height = 40 }: { points: ResourcePoint[]; width?: number; height?: number }) {
  if (points.length < 2) {
    return <div className="resource-empty">采样中…</div>;
  }
  const maxCpu = Math.max(10, ...points.map((p) => p.cpu_pct));
  const stepX = width / (points.length - 1);
  const yFor = (cpu: number) => height - 2 - (Math.min(cpu, maxCpu) / maxCpu) * (height - 4);

  const line = points
    .map((p, i) => `${i === 0 ? "M" : "L"}${(i * stepX).toFixed(1)},${yFor(p.cpu_pct).toFixed(1)}`)
    .join(" ");
  const area = `${line} L${width},${height} L0,${height} Z`;
  const last = points[points.length - 1];

  return (
    <svg width={width} height={height} className="resource-spark">
      <path d={area} fill="rgba(139,92,246,.14)" stroke="none" />
      <path
        d={line}
        fill="none"
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        style={{ stroke: "var(--brand)" }}
      />
      <circle
        cx={width}
        cy={yFor(last.cpu_pct)}
        r={2}
        style={{ fill: "var(--brand)" }}
      />
    </svg>
  );
}
