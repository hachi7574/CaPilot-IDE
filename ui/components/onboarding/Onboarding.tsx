import { useState } from "react";
import { useStore } from "../../state/store";
import { useStructuredStore, createDefaultAgent } from "../../state/structuredAgent";
import { Icon } from "../Icon";

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
  const runtimes = useStore((s) => s.runtimes);
  const setOnboarded = useStore((s) => s.setOnboarded);

  const [step, setStep] = useState(0);
  const [creatingAgent, setCreatingAgent] = useState(false);
  const [agentErr, setAgentErr] = useState<string | null>(null);

  const total = 4;
  const isLast = step === total - 1;
  const isFirst = step === 0;

  /** Create the first agent session, then finish onboarding. Phase 5: new
   *  sessions default to the structured backend (createDefaultAgent). */
  const handleCreateAgent = async () => {
    setCreatingAgent(true);
    setAgentErr(null);
    try {
      if (
        useStore.getState().agents.size > 0 ||
        useStructuredStore.getState().agents.size > 0
      ) {
        setOnboarded(true);
        return;
      }
      await createDefaultAgent();
      setOnboarded(true);
    } catch (e) {
      setAgentErr(`创建失败：${e}。可稍后在左侧栏或底部输入框重试。`);
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
            跳过引导
          </button>
        </div>

        {/* Steps */}
        {step === 0 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">欢迎使用 CaPilot</div>
            <p className="onboarding-step-desc">
              以 IDE 为中心的本地 AI 编码工作台，可统一启动和管理多种 Agent
              CLI 会话。跟随引导完成基础设置，即可开始使用。
            </p>
          </div>
        )}

        {step === 1 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">运行环境检测</div>
            <p className="onboarding-step-desc">
              以下运行时将用于启动 Agent 会话。未登录/未安装的运行时，请按提示
              安装或登录后回到本应用刷新。
            </p>
            <div className="onboarding-runtimes">
              {runtimes.length === 0 && (
                <div className="onboarding-runtime-row">
                  <span>正在检测运行时…</span>
                </div>
              )}
              {runtimes.map((rt) => (
                <div key={rt.id} className="onboarding-runtime-row">
                  <span className="onboarding-runtime-name">{rt.name}</span>
                  <span
                    className="onboarding-runtime-status"
                    data-ok={rt.available}
                  >
                    {rt.available ? (
                      rt.authenticated ? (
                        <>
                          <Icon name="check" size={12} style={{ marginRight: 4 }} /> 已登录
                        </>
                      ) : (
                        <>
                          <Icon name="check" size={12} style={{ marginRight: 4 }} /> 已安装
                        </>
                      )
                    ) : (
                      <>
                        <Icon name="x" size={12} style={{ marginRight: 4 }} /> 未安装
                      </>
                    )}
                  </span>
                </div>
              ))}
            </div>
            <div className="onboarding-guide">
              安装或登录：请参考 <code>docs/</code> 或仓库 README 的运行时配置说明。
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">创建第一个 Agent 会话</div>
            <p className="onboarding-step-desc">
              创建一个结构化 Agent 会话（统一 Timeline 界面），即可在面板内开始工作。
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
                  "正在创建…"
                ) : (
                  <>
                    <Icon name="rocket" size={14} style={{ marginRight: 4 }} />
                    创建 Agent 会话并开始
                  </>
                )}
              </button>
              <div className="onboarding-agent-hint">
                也可以点击下方「下一步」跳过，稍后从项目侧栏或底部输入框创建。
              </div>
            </div>
          </div>
        )}

        {step === 3 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">准备就绪</div>
            <p className="onboarding-step-desc">
              所有基础设置已完成。点击「开始使用」进入 CaPilot IDE；项目旁的
              「+」可新建终端，底部输入框会向当前 Agent 会话发送消息。
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
                上一步
              </button>
            )}
            {isLast ? (
              <button
                className="onboarding-btn onboarding-btn-primary"
                onClick={() => setOnboarded(true)}
              >
                开始使用
              </button>
            ) : (
              <button
                className="onboarding-btn onboarding-btn-primary"
                onClick={() => setStep((s) => s + 1)}
              >
                下一步
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
