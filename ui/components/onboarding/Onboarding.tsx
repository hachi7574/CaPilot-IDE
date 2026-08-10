import { useState } from "react";
import { useStore } from "../../state/store";
import { connectEsp } from "../../state/esp";
import { spawnAgent } from "../../state/agentActions";

/**
 * First-run onboarding overlay (shown until the user completes it).
 * Five steps: 欢迎 → 运行环境检测 → ESP 配对 → 创建会话 → 完成.
 *
 * The whole guide can be skipped at any point via the top-right 跳过引导
 * button; skipping just marks onboarding done and drops the overlay. The
 * If the first session can't be created, an error is shown and the user can
 * still proceed and create one later from the sidebar or composer.
 */
export function Onboarding() {
  const runtimes = useStore((s) => s.runtimes);
  const espStatus = useStore((s) => s.espStatus);
  const espConnecting = useStore((s) => s.espConnecting);
  const setOnboarded = useStore((s) => s.setOnboarded);

  const [step, setStep] = useState(0);
  const [espMsg, setEspMsg] = useState<string | null>(null);
  const [creatingAgent, setCreatingAgent] = useState(false);
  const [agentErr, setAgentErr] = useState<string | null>(null);

  const total = 5;
  const isLast = step === total - 1;
  const isFirst = step === 0;

  const handleEspConnect = async () => {
    setEspMsg(null);
    const err = await connectEsp();
    if (err) setEspMsg(`连接失败：${err}`);
    else setEspMsg("已发起 BLE 连接…");
  };

  /** Create the first agent session, then finish onboarding. */
  const handleCreateAgent = async () => {
    setCreatingAgent(true);
    setAgentErr(null);
    try {
      if (useStore.getState().agents.size > 0) {
        setOnboarded(true);
        return;
      }
      await spawnAgent();
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
                    {rt.available
                      ? rt.authenticated
                        ? "✓ 已登录"
                        : "✓ 已安装"
                      : "✕ 未安装"}
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
            <div className="onboarding-step-title">ESP 遥控器配对</div>
            <p className="onboarding-step-desc">
              通过 BLE 连接 ESP32 遥控器，可远程下发指令与控制。连接状态会显示
              在底部状态栏。你也可以稍后在「设置」中连接。
            </p>
            <div className="onboarding-esp">
              {espStatus.connected ? (
                <div className="onboarding-esp-ok">
                  ✓ 已连接 {espStatus.name ?? "ESP32-C5"}
                  {espStatus.battery_pct !== null &&
                    ` · 🔋 ${espStatus.battery_pct}%`}
                </div>
              ) : (
                <button
                  className="onboarding-btn onboarding-btn-primary"
                  onClick={handleEspConnect}
                  disabled={espConnecting}
                >
                  {espConnecting ? "连接中…" : "连接 ESP (BLE)"}
                </button>
              )}
              {espMsg && (
                <div className="onboarding-esp-msg">{espMsg}</div>
              )}
              {espStatus.connected && espStatus.address && (
                <div className="onboarding-esp-addr">{espStatus.address}</div>
              )}
            </div>
          </div>
        )}

        {step === 3 && (
          <div className="onboarding-step">
            <div className="onboarding-step-title">创建第一个 Agent 会话</div>
            <p className="onboarding-step-desc">
              创建一个 Agent 终端，即可在底部输入框或原生终端界面中开始工作。
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
                {creatingAgent ? "正在创建…" : "🚀 创建 Agent 会话并开始"}
              </button>
              <div className="onboarding-agent-hint">
                也可以点击下方「下一步」跳过，稍后从项目侧栏或底部输入框创建。
              </div>
            </div>
          </div>
        )}

        {step === 4 && (
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
                  setEspMsg(null);
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
