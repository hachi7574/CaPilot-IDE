import { useEffect, useState } from "react";
import { useStore, CiJob } from "../../state/store";
import { useT } from "../../i18n";
import { Icon } from "../Icon";

/**
 * Developer tool: CI build progress for the current version.
 *
 * Polls `ci_status` (GitHub Actions) every few seconds while open and shows the
 * overall progress bar plus one row per job (version-check / build×platform /
 * manifest).
 */
export function CiProgressModal({ onClose }: { onClose: () => void }) {
  const t = useT();
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
        ? { label: t("ci.buildDone"), color: "var(--success)" }
        : { label: t("ci.buildFailed"), color: "var(--danger)" }
      : run.status === "in_progress"
        ? { label: t("ci.building"), color: "var(--warn)" }
        : { label: t("ci.queued"), color: "var(--ink2)" }
    : null;

  return (
    <div className="ci-overlay" onClick={onClose}>
      <div className="ci-card" onClick={(e) => e.stopPropagation()}>
        <div className="ci-head">
          <span className="ci-title">
            <Icon name="loader-circle" size={14} style={{ marginRight: 6 }} />
            {t("ci.title")}
          </span>
          <button className="ci-close" onClick={onClose} title={t("common.close")}>
            <Icon name="close" size={13} />
          </button>
        </div>

        {ciStatus?.error && !run && (
          <div className="ci-error">
            {t("ci.queryFailed", { error: ciStatus.error })}
            <button
              className="ci-retry"
              onClick={pollCiStatus}
              disabled={ciPolling}
            >
              {t("ci.retry")}
            </button>
          </div>
        )}

        {!ciStatus && !ciPolling && <div className="ci-empty">{t("ci.noData")}</div>}

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
                <div className="ci-empty">{t("ci.noJobs")}</div>
              )}
              {run.jobs.map((job) => (
                <CiJobRow key={job.id} job={job} />
              ))}
            </div>

            <div className="ci-foot">
              <span className="ci-foot-title">{run.title}</span>
              <span className="ci-foot-since">
                {since
                  ? t("ci.refreshedAt", {
                      time: new Date(since).toLocaleTimeString(),
                    })
                  : ""}
              </span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function CiJobRow({ job }: { job: CiJob }) {
  const t = useT();
  const done = job.status === "completed";
  const running = job.status === "in_progress";
  const ok = done && job.conclusion === "success";
  const fail = done && job.conclusion !== "success";
  const color = fail ? "var(--danger)" : ok ? "var(--success)" : running ? "var(--warn)" : "var(--ink2)";
  const state = fail
    ? t("ci.jobFailed")
    : ok
      ? t("ci.jobDone")
      : running
        ? t("ci.jobRunning")
        : t("ci.jobWaiting");
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
