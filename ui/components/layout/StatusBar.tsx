import { useEffect, useRef, useState, type MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, ResourcePoint, RuntimeUsage } from "../../state/store";
import { fmtCpu, fmtMem } from "../../state/resource";
import { checkForUpdate, downloadAndInstall } from "../../state/update";
import { Icon, runtimeIcon } from "../Icon";


/** System-wide CPU/MEM snapshot from the backend `system_stats` command. */
interface SystemStats {
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
}

/** Severity color for a resource readout (CPU%/mem%): green → yellow → orange → red. */
function stressColor(pct: number | null | undefined): string {
  if (pct == null) return "var(--ink2)";
  if (pct >= 80) return "var(--danger)";
  if (pct >= 60) return "var(--lane-2)";
  if (pct >= 40) return "var(--warn)";
  return "var(--success)";
}

/** Severity color for remaining usage: the less left, the more urgent. */
function quotaColor(remaining: number | null | undefined): string {
  if (remaining == null) return "var(--ink2)";
  if (remaining < 20) return "var(--danger)";
  if (remaining < 40) return "var(--lane-2)";
  if (remaining < 60) return "var(--warn)";
  return "var(--success)";
}

/** Format a used/total memory pair with a single shared unit, e.g. "5.15 / 14.13GB". */
function formatMemPair(used: number, total: number): string {
  const GB = 1024 ** 3;
  const MB = 1024 ** 2;
  let divisor: number;
  let unit: string;
  if (total >= GB) {
    divisor = GB;
    unit = "GB";
  } else if (total >= MB) {
    divisor = MB;
    unit = "MB";
  } else {
    divisor = 1;
    unit = "B";
  }
  const fmt = (n: number) =>
    divisor === 1 ? String(Math.round(n)) : (n / divisor).toFixed(divisor >= GB ? 2 : 1);
  return `${fmt(used)} / ${fmt(total)}${unit}`;
}

/** Compact countdown for the status bar, e.g. "3d" / "5h" / "12m". */
function formatResetShort(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const d = Math.floor(total / 86400);
  const h = Math.floor((total % 86400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (d > 0) return `${d}d`;
  if (h > 0) return `${h}h`;
  if (m > 0) return `${m}m`;
  return `${total}s`;
}

/** Human countdown for tooltips, e.g. "3天4小时". */
function formatResetCountdown(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const d = Math.floor(total / 86400);
  const h = Math.floor((total % 86400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (d > 0) return `${d}天${h}小时`;
  if (h > 0) return `${h}小时${m}分`;
  if (m > 0) return `${m}分钟`;
  return `${total}秒`;
}

/** Seconds remaining until `resets_at` (unix seconds), or null when unknown. */
function secondsUntilReset(resetsAt: number | null | undefined, nowSec: number): number | null {
  if (resetsAt == null) return null;
  return Math.max(0, resetsAt - nowSec);
}

export function StatusBar() {
  const agents = useStore((s) => s.agents);
  const agentResources = useStore((s) => s.agentResources);
  const usageState = useStore((s) => s.usageState);
  const setUsage = useStore((s) => s.setUsage);
  const currentVersion = useStore((s) => s.currentVersion);
  const updateStatus = useStore((s) => s.updateStatus);
  const updateLatest = useStore((s) => s.updateLatest);
  const updateDownloading = useStore((s) => s.updateDownloading);
  const updateInstallable = useStore((s) => s.updateInstallable);
  const [resourceOpen, setResourceOpen] = useState(false);
  // Tick once a minute so reset countdowns (3d / 5h / 12m) stay honest without
  // re-fetching the remote quota endpoints.
  const [nowSec, setNowSec] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const timer = setInterval(() => setNowSec(Math.floor(Date.now() / 1000)), 60_000);
    return () => clearInterval(timer);
  }, []);

  // Live system-wide CPU/MEM for the readout (2s tick). Always available, so
  // the bar shows the machine's overall state even when no agent is running.
  const [sysStats, setSysStats] = useState<SystemStats | null>(null);
  const resourceAnchorRef = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      invoke<SystemStats>("system_stats")
        .then((s) => {
          if (!cancelled) setSysStats(s);
        })
        .catch(() => {
          if (!cancelled) setSysStats(null);
        });
    };
    tick();
    const timer = setInterval(tick, 2000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  const memText =
    sysStats && sysStats.mem_used != null && sysStats.mem_total
      ? formatMemPair(sysStats.mem_used, sysStats.mem_total)
      : "—";

  const memPct =
    sysStats && sysStats.mem_used != null && sysStats.mem_total
      ? (sysStats.mem_used / sysStats.mem_total) * 100
      : null;

  return (
    <div className="statusbar">
      <span
        ref={resourceAnchorRef}
        className="sb-item resource-item"
        onClick={() => setResourceOpen((o) => !o)}
        style={{ cursor: "pointer", position: "relative" }}
        title="资源监视（系统 CPU / 内存；点击查看全部 agent 曲线）"
      >
        <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
          <span className="sb-name">CPU</span>
          <span className="sb-val" style={{ color: stressColor(sysStats?.cpu_pct) }}>
            {fmtCpu(sysStats?.cpu_pct)}
          </span>
          <span className="sb-name">MEM</span>
          <span className="sb-val" style={{ color: stressColor(memPct) }}>
            {memText}
          </span>
        </span>
        {resourceOpen && (
          <ResourcePopover
            anchorRef={resourceAnchorRef}
            agents={agents}
            agentResources={agentResources}
            onClose={() => setResourceOpen(false)}
          />
        )}
      </span>
      {/* Rate-limit usage readout (Settings → 已安装 → ⚙ → 用量统计). One compact
          item per enabled runtime with a fetchable quota; click to refresh. */}
      {(["codex", "opencode"] as const)
        .filter((rt) => usageState[rt]?.available && usageState[rt].windows.length > 0)
        .map((rt) => (
          <UsageItem
            key={rt}
            runtime={rt}
            usage={usageState[rt]}
            nowSec={nowSec}
            onFetched={(next) => setUsage(rt, next)}
          />
        ))}
      <span className="sb-spacer" />
      {/* Version / update chip — always visible so the user doesn't need to
          open Settings to learn whether an update exists. Click:
          - available → install (or re-show the banner if dismissed)
          - otherwise → run a fresh check */}
      <UpdateStatusItem
        currentVersion={currentVersion}
        status={updateStatus}
        latest={updateLatest}
        downloading={updateDownloading}
        installable={updateInstallable}
      />
    </div>
  );
}

/* ── Version / update chip ───────────────────────────────────── */

function UpdateStatusItem({
  currentVersion,
  status,
  latest,
  downloading,
  installable,
}: {
  currentVersion: string | null;
  status: string;
  latest: string | null;
  downloading: boolean;
  installable: boolean;
}) {
  const hasUpdate = status === "available" && !!latest;
  const label = downloading
    ? "更新中…"
    : hasUpdate
      ? `v${latest} 可更新`
      : status === "checking"
        ? "检查更新…"
        : status === "error"
          ? "更新检查失败"
          : status === "up-to-date"
            ? `v${currentVersion ?? "…"} · 最新`
            : currentVersion
              ? `v${currentVersion}`
              : "版本…";

  const title = downloading
    ? "正在下载并安装更新"
    : hasUpdate
      ? `发现新版本 v${latest}（当前 v${currentVersion ?? "…"}）。点击${installable ? "立即更新" : "查看"}。`
      : status === "error"
        ? "上次检查失败，点击重试"
        : status === "up-to-date"
          ? `已是最新版本 v${currentVersion ?? "…"}。点击重新检查。`
          : "点击检查更新";

  const onClick = (event: MouseEvent) => {
    event.stopPropagation();
    if (downloading) return;
    if (hasUpdate) {
      // Bring the banner back if the user dismissed it earlier this session.
      useStore.setState({ updatePromptDismissedVersion: null });
      if (installable) {
        void downloadAndInstall();
      }
      return;
    }
    void checkForUpdate({ notifyOnFound: true });
  };

  return (
    <button
      type="button"
      className={
        "sb-item sb-update" +
        (hasUpdate || downloading ? " is-update" : "") +
        (status === "error" ? " is-warn" : "")
      }
      onClick={onClick}
      title={title}
      disabled={downloading || status === "checking"}
      aria-label={title}
    >
      <Icon
        name={hasUpdate || downloading ? "download" : "refresh-cw"}
        size={12}
      />
      <span className="sb-val">{label}</span>
    </button>
  );
}

/* ── Rate-limit usage item ────────────────────────────────────── */

function UsageItem({
  runtime,
  usage,
  nowSec,
  onFetched,
}: {
  runtime: "codex" | "opencode";
  usage: RuntimeUsage;
  nowSec: number;
  onFetched: (usage: RuntimeUsage) => void;
}) {
  const [refreshing, setRefreshing] = useState(false);
  const displayName = runtime === "codex" ? "Codex" : "OpenCode";
  // Headline the 7d/weekly window when present (the meaningful quota); codex has
  // only that window, opencode reports rolling(5h)/weekly(7d)/monthly(30d).
  const primary = usage.windows.find((w) => w.label === "7d") ?? usage.windows[0];
  const remaining = primary?.remaining_pct ?? null;
  const resetLeft = secondsUntilReset(primary?.resets_at, nowSec);
  // Prefer real reset countdown ("3d"/"5h"/"12m") over the static window label
  // ("7d") so the bar answers "when does this refill?" at a glance.
  const resetText = resetLeft != null ? formatResetShort(resetLeft) : null;
  const label =
    remaining != null
      ? `${resetText ?? primary.label} ${Math.round(remaining)}%`
      : resetText ?? primary?.label ?? "";

  // Full breakdown for the tooltip: each window's used/remaining + countdown.
  const lines = usage.windows.map((w) => {
    const used = w.used_pct != null ? `已用 ${Math.round(w.used_pct)}%` : null;
    const rem = w.remaining_pct != null ? `剩余 ${Math.round(w.remaining_pct)}%` : null;
    const left = secondsUntilReset(w.resets_at, nowSec);
    const reset = left != null ? `重置还有 ${formatResetCountdown(left)}` : null;
    return `${w.label} · ${[used, rem].filter(Boolean).join(" / ")}${reset ? ` · ${reset}` : ""}`;
  });
  const plan = usage.plan_type ? `（${usage.plan_type}）` : "";
  const tooltip = [`${displayName}${plan}`, ...lines, refreshing ? "刷新中…" : "点击立即刷新"].join("\n");

  const handleRefresh = async (event: MouseEvent) => {
    // Stop the status-bar resource popover / other siblings from reacting.
    event.stopPropagation();
    if (refreshing) return;
    setRefreshing(true);
    try {
      const next = await invoke<RuntimeUsage>("usage_fetch", { runtime });
      onFetched(next);
    } catch {
      // Keep the last known value; tooltip still invites another try.
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <button
      type="button"
      className={`sb-item sb-usage${refreshing ? " is-refreshing" : ""}`}
      onClick={handleRefresh}
      title={tooltip}
      disabled={refreshing}
      aria-label={`${displayName} 用量，点击刷新`}
    >
      <Icon
        name={runtime === "codex" ? "openai" : "opencode"}
        size={13}
        style={{ color: "var(--ink2)" }}
      />
      <span className="sb-val" style={{ color: quotaColor(remaining) }}>
        {label}
      </span>
    </button>
  );
}

/* ── Resource popover (DevPlan §10) ──────────────────────────── */

function ResourcePopover({
  anchorRef,
  agents,
  agentResources,
  onClose,
}: {
  anchorRef: { current: HTMLSpanElement | null };
  agents: Map<string, AgentInfo>;
  agentResources: Map<string, ResourcePoint>;
  onClose: () => void;
}) {
  // Anchor the popover centered above the status-bar item, clamped inside the
  // viewport so a narrow window can't push it off-screen.
  const [pos, setPos] = useState<{ x: number; bottom: number }>(() => measure());
  useEffect(() => {
    // Reset every 200ms while open: the agent list grows/shrinks and the window
    // can resize, so a stale position would drift off the anchor.
    const timer = setInterval(() => setPos(measure()), 200);
    return () => clearInterval(timer);
  }, []);

  function measure() {
    const anchor = anchorRef.current;
    if (!anchor) return { x: 8, bottom: 40 };
    const r = anchor.getBoundingClientRect();
    const w = 320;
    const x = Math.max(8, Math.min(r.left + r.width / 2 - w / 2, window.innerWidth - w - 8));
    return { x, bottom: window.innerHeight - r.top + 10 };
  }

  // Restored and ended sessions remain in the sidebar, but have no live PTY.
  // Listing them here produced a large wall of misleading `CPU — / MEM —` rows.
  const rows = [...agents.values()].filter(
    (agent) => agent.pid !== null && agent.status !== "done"
  );

  return (
    <div
      className="resource-popover"
      onMouseLeave={onClose}
      style={{ left: pos.x, bottom: pos.bottom }}
    >
      <div className="resource-popover-title">
        <span style={{ display: "inline-flex", alignItems: "center" }}>
          <Icon name="settings" size={13} style={{ marginRight: 4 }} />
          资源监视
        </span>
      </div>
      <div className="resource-list">
        {rows.length === 0 && (
          <div className="resource-empty">没有运行中的 agent</div>
        )}
        {rows.map((a) => {
          const r = agentResources.get(a.id);
          return (
            <div className="resource-row" key={a.id}>
              <span className="resource-name" title={a.id}>
                <Icon name={runtimeIcon(a.runtime)} size={12} style={{ flex: "none", color: "var(--ink2)" }} />
                <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                  {a.title || a.id.slice(0, 6)}
                </span>
              </span>
              <span className="resource-val">CPU {fmtCpu(r?.cpu_pct)}</span>
              <span className="resource-val">MEM {fmtMem(r?.mem_bytes)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
