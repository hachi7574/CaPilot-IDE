import { useState } from "react";
import { useStore } from "../../state/store";
import { spawnAgent } from "../../state/agentActions";
import { isWindowsHost } from "../../state/shellPath";
import { Icon } from "../Icon";
import { useT } from "../../i18n";

/**
 * First-run onboarding overlay (shown until the user completes it).
 * Four steps: 欢迎 → 运行环境检测 → 创建会话 → 完成.
 *
 * The whole guide can be skipped at any point via the top-right 跳过引导
 * button; skipping just marks onboarding done and drops the overlay. The
 * If the first session can't be created, an error is shown and the user can
 * still proceed and create one later from the sidebar or composer.
 */
export function Onboarding() {
  const t = useT();
  const runtimes = useStore((s) => s.runtimes);
  const setOnboarded = useStore((s) => s.setOnboarded);

  const [step, setStep] = useState(0);
  const [creatingAgent, setCreatingAgent] = useState(false);
  const [agentErr, setAgentErr] = useState<string | null>(null);

  const total = 4;
  const isLast = step === total - 1;
  const isFirst = step === 0;

  /** Create the first agent session, then finish onboarding. The spawn is
   *  race-guarded so a hung backend can't leave the button stuck on
   *  "正在创建…" (disabled) forever — after the timeout the error path runs and
   *  the button becomes clickable again. */
  const handleCreateAgent = async () => {
    setCreatingAgent(true);
    setAgentErr(null);
    try {
      if (useStore.getState().agents.size > 0) {
        setOnboarded(true);
        return;
      }
      await Promise.race([
        spawnAgent(),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error(t("onboarding.createTimeout"))), 15000)
        ),
      ]);
      setOnboarded(true);
    } catch (e) {
      setAgentErr(t("onboarding.createFailed", { err: String(e) }));
      setCreatingAgent(false);
    }
  };

  return (
    <div className="onboarding-overlay">
      <div className="onboarding-card">
        {/* Header: logo + title + skip */}
        <div className="onboarding-header">
          <img src="/logo.png" alt="CaPilot" className="onboarding-logo" />
          <h2>CaPilot IDE</h2>
          <button
            className="onboarding-skip"
            onClick={() => setOnboarded(true)}
          >
            {t("onboarding.skip")}
          </button>
        </div>

        {/* Steps */}
        {step === 0 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">{t("onboarding.welcome")}</div>
            <p className="onboarding-step-desc">
              {t("onboarding.welcomeDesc")}
            </p>
          </div>
        )}

        {step === 1 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">{t("onboarding.runtimeDetect")}</div>
            <p className="onboarding-step-desc">
              {t("onboarding.runtimeDesc")}
            </p>
            <div className="onboarding-runtimes">
              {runtimes.length === 0 && (
                <div className="onboarding-runtime-row">
                  <span>{t("onboarding.detecting")}</span>
                </div>
              )}
              {runtimes
                .filter(
                  (rt) =>
                    isWindowsHost() ||
                    (rt.id !== "powershell" && rt.id !== "cmd")
                )
                .map((rt) => (
                <div key={rt.id} className="onboarding-runtime-row">
                  <span className="onboarding-runtime-name">{rt.name}</span>
                  <span
                    className="onboarding-runtime-status"
                    data-ok={rt.available}
                  >
                    {rt.available ? (
                      rt.authenticated ? (
                        <>
                          <Icon name="check" size={12} style={{ marginRight: 4 }} /> {t("onboarding.loggedIn")}
                        </>
                      ) : (
                        <>
                          <Icon name="check" size={12} style={{ marginRight: 4 }} /> {t("onboarding.installed")}
                        </>
                      )
                    ) : (
                      <>
                        <Icon name="x" size={12} style={{ marginRight: 4 }} /> {t("onboarding.notInstalled")}
                      </>
                    )}
                  </span>
                </div>
              ))}
            </div>
            <div className="onboarding-guide">
              {t("onboarding.installGuide")}
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">{t("onboarding.createSession")}</div>
            <p className="onboarding-step-desc">
              {t("onboarding.createSessionDesc")}
            </p>
            <div className="onboarding-agent">
              {agentErr && (
                <div className="onboarding-agent-err">{agentErr}</div>
              )}
              <button
                className="onboarding-btn onboarding-btn-primary"
                onClick={handleCreateAgent}
                disabled={creatingAgent}
              >
                {creatingAgent ? (
                  t("onboarding.creating")
                ) : (
                  <>
                    <Icon name="rocket" size={14} style={{ marginRight: 4 }} />
                    {t("onboarding.createAndStart")}
                  </>
                )}
              </button>
              <div className="onboarding-agent-hint">
                {t("onboarding.createHint")}
              </div>
            </div>
          </div>
        )}

        {step === 3 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">{t("onboarding.ready")}</div>
            <p className="onboarding-step-desc">
              {t("onboarding.readyDesc")}
            </p>
          </div>
        )}

        {/* Footer: progress + nav */}
        <div className="onboarding-footer">
          <div className="onboarding-dots">
            {Array.from({ length: total }).map((_, i) => (
              <span
                key={i}
                className={`onboarding-dot${i === step ? " active" : ""}`}
              />
            ))}
          </div>
          <div className="onboarding-nav">
            {!isFirst && (
              <button
                className="onboarding-btn"
                onClick={() => {
                  setStep((s) => s - 1);
                  setAgentErr(null);
                }}
              >
                {t("onboarding.prev")}
              </button>
            )}
            {isLast ? (
              <button
                className="onboarding-btn onboarding-btn-primary"
                onClick={() => setOnboarded(true)}
              >
                {t("onboarding.start")}
              </button>
            ) : (
              <button
                className="onboarding-btn onboarding-btn-primary"
                onClick={() => setStep((s) => s + 1)}
              >
                {t("onboarding.next")}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
