import { useEffect, useState } from "react";
import { useStore, CiJob } from "../../state/store";
import { Icon } from "../Icon";

/**
 * Developer tool: CI build progress for the current version.
 *
 * Polls `ci_status` (GitHub Actions) every few seconds while open and shows the
 * overall progress bar plus one row per job (version-check / build×platform /
 * manifest). Clicking a running build's "在 GitHub 查看" opens the run in a
 * browser via the opener plugin.
 */
export function CiProgressModal({ onClose }: { onClose: () => void }) {
  const ciStatus = useStore((s) => s.ciStatus);
  const ciPolling = useStore((s) => s.ciPolling);
  const pollCiStatus = useStore((s) => s.pollCiStatus);
  const [since, setSince] = useState<number | null>(null);

  // Poll on mount, then keep refreshing every 5s while open.
  useEffect(() => {
    pollCiStatus();
    setSince(Date.now());
    const timer = setInterval(pollCiStatus, 5000);
    return () => clearInterval(timer);
  }, [pollCiStatus]);

  useEffect(() => {
    const closeOnEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  const run = ciStatus?.run ?? null;
  const progress = run ? Math.round((run.progress ?? 0) * 100) : 0;

  const runState = run
    ? run.status === "completed"
      ? run.conclusion === "success"
        ? { label: "构建完成", color: "var(--success)" }
        : { label: "构建失败", color: "var(--danger)" }
      : run.status === "in_progress"
        ? { label: "构建中…", color: "var(--warn)" }
        : { label: "排队中…", color: "var(--ink2)" }
    : null;

  return (
    <div className="ci-overlay" onClick={onClose}>
      <div className="ci-card" onClick={(e) => e.stopPropagation()}>
        <div className="ci-head">
          <span className="ci-title">
            <Icon name="loader-circle" size={14} style={{ marginRight: 6 }} />
            CI 构建进度
          </span>
          <button className="ci-close" onClick={onClose} title="关闭">
            <Icon name="close" size={13} />
          </button>
        </div>

        {ciStatus?.error && !run && (
          <div className="ci-error">
            无法查询 CI 状态：{ciStatus.error}
            <button
              className="ci-retry"
              onClick={pollCiStatus}
              disabled={ciPolling}
            >
              重试
            </button>
          </div>
        )}

        {!ciStatus && !ciPolling && <div className="ci-empty">暂无数据</div>}

        {run && (
          <>
            <div className="ci-tag">
              <span className="ci-tag-name">{ciStatus?.tag ?? ""}</span>
              {runState && (
                <span className="ci-run-state" style={{ color: runState.color }}>
                  {runState.label}
                </span>
              )}
              <span className="ci-progress-pct">{progress}%</span>
            </div>

            <div className="ci-progress">
              <div
                className="ci-progress-fill"
                style={{
                  width: `${progress}%`,
                  background: runState?.color ?? "var(--brand)",
                }}
              />
            </div>

            <div className="ci-jobs">
              {run.jobs.length === 0 && (
                <div className="ci-empty">还没有 job 记录（构建尚未启动）</div>
              )}
              {run.jobs.map((job) => (
                <CiJobRow key={job.id} job={job} />
              ))}
            </div>

            <div className="ci-foot">
              <span className="ci-foot-title">{run.title}</span>
              <span className="ci-foot-since">
                {since ? `刷新于 ${new Date(since).toLocaleTimeString()}` : ""}
              </span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function CiJobRow({ job }: { job: CiJob }) {
  const done = job.status === "completed";
  const running = job.status === "in_progress";
  const ok = done && job.conclusion === "success";
  const fail = done && job.conclusion !== "success";
  const color = fail ? "var(--danger)" : ok ? "var(--success)" : running ? "var(--warn)" : "var(--ink2)";
  const state = fail
    ? "失败"
    : ok
      ? "完成"
      : running
        ? "运行中"
        : "等待中";
  return (
    <div className="ci-job">
      <Icon
        name={fail ? "circle-slash" : ok ? "circle-check" : running ? "loader-circle" : "circle"}
        size={13}
        style={{ color, flex: "none" }}
      />
      <span className="ci-job-name">{job.name}</span>
      <span className="ci-job-state" style={{ color }}>
        {state}
      </span>
    </div>
  );
}
