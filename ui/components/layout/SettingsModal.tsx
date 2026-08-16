import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { useStore, FontScale, RuntimeInfo, UsageConfig } from "../../state/store";
import { checkForUpdate, downloadAndInstall } from "../../state/update";
import { Icon } from "../Icon";

interface SettingsModalProps {
  onClose: () => void;
}

/** Adapter defaults, mirrored from the Rust spawn_interactive() implementations.
 *  Shown in the launch editor when the user has not overridden a runtime. */
const DEFAULT_LAUNCH: Record<string, { command: string; args: string }> = {
  claude: { command: "claude", args: "--model claude-sonnet-5" },
  codex: { command: "codex", args: "--no-alt-screen" },
  opencode: { command: "opencode", args: "" },
  // dsh 走 dsh-tui profile；每会话的 model/effort/恢复经 `--patch <临时文件>`
  // 注入（spawn 时由适配器追加），编辑器只展示静态前缀。
  dsh: { command: "dsh", args: "--profile dsh-tui" },
  // pi 每会话的 model/thinking/mode 由适配器在 spawn 时按 flag 追加。
  pi: { command: "pi", args: "" },
};

export function SettingsModal({ onClose }: SettingsModalProps) {
  const runtimes = useStore((s) => s.runtimes);
  const setRuntimes = useStore((s) => s.setRuntimes);
  const setOnboarded = useStore((s) => s.setOnboarded);
  const fontScale = useStore((s) => s.fontScale);
  const setFontScale = useStore((s) => s.setFontScale);

  // App self-update slice (docs/version-update-design.md).
  const currentVersion = useStore((s) => s.currentVersion);
  const updateStatus = useStore((s) => s.updateStatus);
  const updateLatest = useStore((s) => s.updateLatest);
  const updateNotes = useStore((s) => s.updateNotes);
  const updateError = useStore((s) => s.updateError);
  const updateDownloading = useStore((s) => s.updateDownloading);
  const updateProgress = useStore((s) => s.updateProgress);
  const updateInstallable = useStore((s) => s.updateInstallable);
  const autoCheckUpdate = useStore((s) => s.autoCheckUpdate);
  const setAutoCheckUpdate = useStore((s) => s.setAutoCheckUpdate);

  // If the About panel opens before useUpdateSync has resolved the bundle
  // version, fetch it here so v{…} never shows a stale hardcoded number.
  useEffect(() => {
    if (currentVersion) return;
    getVersion()
      .then((v) => useStore.setState({ currentVersion: v }))
      .catch(() => {});
  }, [currentVersion]);

  const [scanning, setScanning] = useState(false);
  const reDetect = () => {
    setScanning(true);
    invoke<RuntimeInfo[]>("runtime_list_available")
      .then((runtimes) => {
        setRuntimes(Array.isArray(runtimes) ? runtimes : []);
      })
      .catch(() => {})
      .finally(() => setScanning(false));
  };

  // Session-end handling: "keep" (default) marks a naturally-exited session done
  // (recoverable from the sidebar's "已结束" group); "delete" removes it.
  const [sessionEndMode, setSessionEndMode] = useState<"keep" | "delete">("keep");
  useEffect(() => {
    invoke<string | null>("setting_get", { key: "session_end_mode" })
      .then((v) => {
        if (v) setSessionEndMode(v === "delete" ? "delete" : "keep");
      })
      .catch(() => {
        // Backend not ready — keep default.
      });
  }, []);

  const changeSessionEndMode = async (mode: "keep" | "delete") => {
    setSessionEndMode(mode);
    await invoke("setting_set", { key: "session_end_mode", value: mode }).catch(
      () => {}
    );
  };

  const reShowOnboarding = () => {
    setOnboarded(false);
    onClose();
  };

  // Per-runtime launch overrides (Settings → 已安装 → ⚙): edits the binary and
  // args used to spawn an agent session. Persisted as a JSON map in the same KV
  // store as other settings; empty fields fall back to adapter defaults.
  const [overrides, setOverrides] = useState<
    Record<string, { command: string; args: string }>
  >({});
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editCmd, setEditCmd] = useState("");
  const [editArgs, setEditArgs] = useState("");

  // Rate-limit usage stats (Settings → 已安装 → ⚙ → 用量统计): enable flag +
  // per-runtime fetch config, persisted in the settings KV. The status bar reads
  // the fetched result via store.usageState; this modal only edits config.
  const [usageEnabled, setUsageEnabled] = useState<Record<string, boolean>>({});
  const [usageConfig, setUsageConfig] = useState<Record<string, UsageConfig>>({});
  const [usageCookie, setUsageCookie] = useState("");
  const [usageWorkspace, setUsageWorkspace] = useState("");
  const [usageChecks, setUsageChecks] = useState<
    Record<string, { ok: boolean; message: string } | null>
  >({});
  const [checkingId, setCheckingId] = useState<string | null>(null);
  const usageState = useStore((s) => s.usageState);
  const bumpUsageRevision = useStore((s) => s.bumpUsageRevision);

  useEffect(() => {
    invoke<string | null>("setting_get", { key: "usage_enabled" })
      .then((raw) => {
        if (raw) {
          try {
            setUsageEnabled(JSON.parse(raw));
          } catch {
            // malformed — keep defaults
          }
        }
      })
      .catch(() => {});
    invoke<string | null>("setting_get", { key: "usage_config" })
      .then((raw) => {
        if (raw) {
          try {
            setUsageConfig(JSON.parse(raw));
          } catch {
            // malformed — keep defaults
          }
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    invoke<string | null>("setting_get", { key: "runtime_overrides" })
      .then((raw) => {
        if (!raw) return;
        try {
          const parsed = JSON.parse(raw) as Record<
            string,
            { command: string; args: string }
          >;
          if (parsed && typeof parsed === "object") setOverrides(parsed);
        } catch {
          // malformed JSON — keep defaults
        }
      })
      .catch(() => {
        // backend may still be starting
      });
  }, []);

  const persistOverrides = (
    next: Record<string, { command: string; args: string }>
  ) => {
    setOverrides(next);
    invoke("setting_set", {
      key: "runtime_overrides",
      value: JSON.stringify(next),
    }).catch(() => {});
  };

  const toggleEditor = (id: string) => {
    // Clicking the gear of the already-open editor saves and collapses.
    if (editingId === id) {
      saveOverride();
      return;
    }
    // Prefill with the currently effective launch line: the user override when
    // present, otherwise the runtime's default command/args.
    const effective = overrides[id] ?? DEFAULT_LAUNCH[id] ?? { command: "", args: "" };
    setEditingId(id);
    setEditCmd(effective.command);
    setEditArgs(effective.args);
    // Prefill the rate-limit usage config (cookie / workspace id) for this runtime.
    const cfg = usageConfig[id];
    setUsageCookie(cfg?.auth_cookie ?? "");
    setUsageWorkspace(cfg?.workspace_id ?? "");
  };

  const saveOverride = () => {
    if (!editingId) return;
    const next = { ...overrides };
    const command = editCmd.trim();
    const args = editArgs.trim();
    if (command || args) {
      next[editingId] = { command, args };
    } else {
      delete next[editingId];
    }
    setEditingId(null);
    persistOverrides(next);
  };

  const resetOverride = (id: string) => {
    const next = { ...overrides };
    delete next[id];
    persistOverrides(next);
  };

  // ── Rate-limit usage helpers ──────────────────────────────────

  const persistUsageEnabled = (id: string, value: boolean) => {
    const next = { ...usageEnabled, [id]: value };
    setUsageEnabled(next);
    invoke("setting_set", {
      key: "usage_enabled",
      value: JSON.stringify(next),
    }).catch(() => {});
    bumpUsageRevision();
  };

  const updateUsageConfig = (id: string, patch: Partial<UsageConfig>) => {
    const next = { ...usageConfig, [id]: { ...usageConfig[id], ...patch } };
    setUsageConfig(next);
    invoke("setting_set", {
      key: "usage_config",
      value: JSON.stringify(next),
    }).catch(() => {});
    bumpUsageRevision();
  };

  const runUsageCheck = async (id: string) => {
    setCheckingId(id);
    setUsageChecks((c) => ({ ...c, [id]: null }));
    try {
      const res = await invoke<{ available: boolean; message: string }>(
        "usage_check",
        { runtime: id }
      );
      setUsageChecks((c) => ({ ...c, [id]: { ok: res.available, message: res.message } }));
    } catch (e) {
      setUsageChecks((c) => ({ ...c, [id]: { ok: false, message: String(e) } }));
    } finally {
      setCheckingId(null);
    }
  };

  // The "已安装" list shows only installed agent runtimes — plain shells
  // (bash) are excluded.
  const installedAgents = runtimes.filter(
    (rt) => rt.available && !rt.id.startsWith("bash")
  );

  return (
    <div
      className="modal-overlay"
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgb(var(--black-rgb) / 0.6)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 2000,
      }}
    >
      <div
        className="modal"
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--bg2)",
          border: "2px solid var(--rule2)",
          width: 460,
          maxWidth: "90vw",
          maxHeight: "80vh",
          overflowY: "auto",
          padding: 20,
        }}
      >
        <div className="modal-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 16 }}>
          <h3 style={{ fontFamily: "var(--pixel-body)", fontSize: "var(--fs-xl)", color: "var(--brand)", letterSpacing: 1 }}>
            <Icon name="settings" size={18} style={{ marginRight: 6 }} /> Settings
          </h3>
          <button className="modal-close" onClick={onClose} style={{ background: "none", border: "none", color: "var(--ink2)", fontSize: "var(--fs-lg)", cursor: "pointer" }}>
            ×
          </button>
        </div>

        {/* Installed agents */}
        <div className="modal-section" style={{ marginBottom: 24 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
            <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-sm)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase" }}>
              已安装
            </div>
            <button
              onClick={reDetect}
              disabled={scanning}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "4px 12px",
                border: "1px solid var(--rule2)",
                color: "var(--ink2)",
                background: "transparent",
                borderRadius: 6,
                cursor: "pointer",
              }}
            >
              {scanning ? "检测中…" : "重新检测"}
            </button>
          </div>
          {installedAgents.length === 0 && (
            <div style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>未检测到已安装的 Agent</div>
          )}
          {installedAgents.map((rt) => (
            <div key={rt.id} style={{ borderBottom: "1px solid var(--rule)" }}>
              <div className="modal-row" style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "8px 0", fontSize: "var(--fs-sm)" }}>
                <span style={{ color: "var(--ink2)" }}>{rt.name}</span>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ color: "var(--success)", fontFamily: "var(--mono)", fontSize: "var(--fs-xs)" }}>
                    <Icon name="check" size={12} style={{ marginRight: 4 }} />
                    {rt.authenticated ? "已登录" : "已安装"}
                  </span>
                  <button
                    onClick={() => toggleEditor(rt.id)}
                    style={{
                      display: "flex", alignItems: "center", justifyContent: "center",
                      width: 22, height: 22, borderRadius: 6, cursor: "pointer",
                      background: editingId === rt.id ? "rgb(var(--brand-rgb) / .1)" : "transparent",
                      border: `1px solid ${editingId === rt.id ? "var(--brand)" : "var(--rule2)"}`,
                      color: editingId === rt.id ? "var(--brand)" : "var(--ink2)",
                    }}
                    title="点击展开/收起启动命令配置（收起即保存）"
                  >
                    <Icon name="settings" size={12} />
                  </button>
                </div>
              </div>
              {editingId === rt.id && (
                <div style={{ padding: "0 0 10px", display: "flex", flexDirection: "column", gap: 6 }}>
                  <div>
                    <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                      启动命令
                    </div>
                    <input
                      className="modal-text-input"
                      value={editCmd}
                      onChange={(e) => setEditCmd(e.target.value)}
                      onKeyDown={(e) => { if (e.key === "Enter") saveOverride(); }}
                      placeholder={`例如 ${rt.id}`}
                    />
                  </div>
                  <div>
                    <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                      命令参数
                    </div>
                    <input
                      className="modal-text-input"
                      value={editArgs}
                      onChange={(e) => setEditArgs(e.target.value)}
                      onKeyDown={(e) => { if (e.key === "Enter") saveOverride(); }}
                      placeholder="留空使用默认参数（空格分隔）"
                    />
                  </div>
                  <div style={{ display: "flex", gap: 6, marginTop: 2 }}>
                    <button
                      onClick={saveOverride}
                      style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--brand)", color: "var(--brand)", background: "rgb(var(--brand-rgb) / .08)", borderRadius: 6, cursor: "pointer" }}
                    >
                      保存
                    </button>
                    <button
                      onClick={() => setEditingId(null)}
                      style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--rule2)", color: "var(--ink2)", background: "transparent", borderRadius: 6, cursor: "pointer" }}
                    >
                      取消
                    </button>
                    {overrides[rt.id] && (
                      <button
                        onClick={() => resetOverride(rt.id)}
                        style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--rule2)", color: "var(--warn)", background: "transparent", borderRadius: 6, cursor: "pointer" }}
                      >
                        恢复默认
                      </button>
                    )}
                  </div>

                  {/* Rate-limit usage stats (codex/opencode only) */}
                  {(rt.id === "codex" || rt.id === "opencode") && (
                    <div style={{ borderTop: "1px solid var(--rule)", marginTop: 10, paddingTop: 8 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 4, fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
                        <Icon name="activity" size={11} /> 用量统计（剩余用量）
                      </div>

                      {/* Enable toggle */}
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8, fontSize: "var(--fs-2xs)", color: "var(--ink2)" }}>
                        <span>启用：状态栏显示剩余用量</span>
                        <button
                          onClick={() => persistUsageEnabled(rt.id, !usageEnabled[rt.id])}
                          style={{
                            fontFamily: "var(--pixel)",
                            fontSize: "var(--fs-2xs)",
                            padding: "4px 12px",
                            border: `1px solid ${usageEnabled[rt.id] ? "var(--brand)" : "var(--rule2)"}`,
                            color: usageEnabled[rt.id] ? "var(--brand)" : "var(--ink2)",
                            background: usageEnabled[rt.id] ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                            borderRadius: 6,
                            cursor: "pointer",
                          }}
                        >
                          {usageEnabled[rt.id] ? "已启用" : "已停用"}
                        </button>
                      </div>

                      {/* opencode: cookie + workspace id */}
                      {rt.id === "opencode" && (
                        <>
                          <div style={{ marginBottom: 6 }}>
                            <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                              opencode.ai 登录 Cookie（以 auth= 开头）
                            </div>
                            <input
                              className="modal-text-input"
                              type="password"
                              value={usageCookie}
                              onChange={(e) => {
                                setUsageCookie(e.target.value);
                                updateUsageConfig("opencode", { auth_cookie: e.target.value });
                              }}
                              placeholder="浏览器登录 opencode.ai 后复制 auth cookie"
                            />
                          </div>
                          <div style={{ marginBottom: 6 }}>
                            <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                              Workspace ID（可留空自动探测）
                            </div>
                            <input
                              className="modal-text-input"
                              value={usageWorkspace}
                              onChange={(e) => {
                                setUsageWorkspace(e.target.value);
                                updateUsageConfig("opencode", { workspace_id: e.target.value });
                              }}
                              placeholder="https://opencode.ai/workspace/<id>/go 中的 <id>"
                            />
                          </div>
                        </>
                      )}

                      {/* codex: no config (auth auto-discovered) */}
                      {rt.id === "codex" && (
                        <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 8 }}>
                          自动读取 ~/.codex/auth.json 登录态，无需配置。
                          当前：<span style={{ color: rt.authenticated ? "var(--success)" : "var(--warn)" }}>
                            {rt.authenticated ? "已登录" : "未登录"}
                          </span>
                        </div>
                      )}

                      {/* Availability check */}
                      <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", marginBottom: 4 }}>
                        <button
                          onClick={() => runUsageCheck(rt.id)}
                          disabled={checkingId === rt.id}
                          style={{
                            display: "flex", alignItems: "center", gap: 4,
                            fontFamily: "var(--pixel)",
                            fontSize: "var(--fs-2xs)",
                            padding: "4px 12px",
                            border: "1px solid var(--rule2)",
                            color: "var(--ink2)",
                            background: "transparent",
                            borderRadius: 6,
                            cursor: checkingId === rt.id ? "default" : "pointer",
                          }}
                        >
                          <Icon name="refresh-cw" size={11} />
                          {checkingId === rt.id ? "检查中…" : "检查可用性"}
                        </button>
                        {(() => {
                          const uc = usageChecks[rt.id];
                          if (!uc) return null;
                          return (
                            <span
                              style={{
                                display: "inline-flex", alignItems: "center", gap: 4,
                                fontSize: "var(--fs-2xs)",
                                color: uc.ok ? "var(--success)" : "var(--warn)",
                              }}
                            >
                              <Icon name={uc.ok ? "circle-check" : "triangle-alert"} size={12} />
                              {uc.message}
                            </span>
                          );
                        })()}
                      </div>

                      {/* Live status-bar summary */}
                      {usageState[rt.id]?.available && (
                        <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)" }}>
                          状态栏显示：{usageState[rt.id].windows
                            .map((w) => `${w.label} ${w.remaining_pct != null ? `剩余 ${Math.round(w.remaining_pct)}%` : ""}`.trim())
                            .join(" · ")}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>

        {/* Preferences */}
        <div className="modal-section" style={{ marginBottom: 16 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-sm)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            通用偏好
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", fontSize: "var(--fs-sm)" }}>
            <span style={{ color: "var(--ink2)" }}>界面字体大小</span>
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            {(
              [
                { key: "s", label: "最小" },
                { key: "m", label: "小" },
                { key: "l", label: "中" },
                { key: "xl", label: "大" },
                { key: "xxl", label: "最大" },
              ] as { key: FontScale; label: string }[]
            ).map((opt) => (
              <button
                key={opt.key}
                onClick={() => setFontScale(opt.key)}
                style={{
                  fontFamily: "var(--pixel)",
                  fontSize: "var(--fs-2xs)",
                  padding: "4px 10px",
                  border: `1px solid ${fontScale === opt.key ? "var(--brand)" : "var(--rule2)"}`,
                  color: fontScale === opt.key ? "var(--brand)" : "var(--ink2)",
                  background: fontScale === opt.key ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                  cursor: "pointer",
                }}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </div>

        {/* Session end handling */}
        <div className="modal-section" style={{ marginBottom: 16, borderTop: "1px solid var(--rule)", paddingTop: 10 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-sm)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            会话
          </div>
          <div style={{ fontSize: "var(--fs-xs)", color: "var(--ink2)", marginBottom: 8 }}>
            进程自然退出后（终端里 exit / 任务跑完 / Ctrl+D，不是点 × 关闭）
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              onClick={() => changeSessionEndMode("keep")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "6px 12px",
                border: `1px solid ${sessionEndMode === "keep" ? "var(--brand)" : "var(--rule2)"}`,
                color: sessionEndMode === "keep" ? "var(--brand)" : "var(--ink2)",
                background: sessionEndMode === "keep" ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                cursor: "pointer",
              }}
            >
              保留并标记已结束（侧栏可找回）
            </button>
            <button
              onClick={() => changeSessionEndMode("delete")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "6px 12px",
                border: `1px solid ${sessionEndMode === "delete" ? "var(--brand)" : "var(--rule2)"}`,
                color: sessionEndMode === "delete" ? "var(--brand)" : "var(--ink2)",
                background: sessionEndMode === "delete" ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                cursor: "pointer",
              }}
            >
              直接删除
            </button>
          </div>
        </div>

        {/* First-run onboarding */}
        <div className="modal-section" style={{ marginBottom: 16, borderTop: "1px solid var(--rule)", paddingTop: 10 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", fontSize: "var(--fs-sm)" }}>
            <span style={{ color: "var(--ink2)" }}>重新显示引导流程</span>
            <button
              onClick={reShowOnboarding}
              className="modal-btn"
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "5px 12px",
                border: "1px solid var(--brand)",
                color: "var(--brand)",
                background: "transparent",
                cursor: "pointer",
              }}
            >
              重新显示
            </button>
          </div>
        </div>

        {/* About / Updates */}
        <div className="modal-section" style={{ borderTop: "1px solid var(--rule)", paddingTop: 10 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-sm)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            关于与更新
          </div>
          <div style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>
            CaPilot IDE <span style={{ fontFamily: "var(--mono)", color: "var(--ink2)" }}>v{currentVersion ?? "…"}</span>
          </div>
          <div style={{ fontSize: "var(--fs-xs)", color: "var(--ink2)", marginTop: 4, fontFamily: "var(--mono)" }}>
            Local AI coding workspace
          </div>

          {/* Version check row */}
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", marginTop: 12 }}>
            <button
              onClick={() => checkForUpdate()}
              disabled={updateStatus === "checking" || updateDownloading}
              style={{
                display: "flex", alignItems: "center", gap: 4,
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "4px 12px",
                border: "1px solid var(--rule2)",
                color: "var(--ink2)",
                background: "transparent",
                borderRadius: 6,
                cursor: updateStatus === "checking" || updateDownloading ? "default" : "pointer",
              }}
            >
              <Icon name="refresh-cw" size={11} />
              {updateStatus === "checking" ? "检查中…" : "检查更新"}
            </button>
            {updateStatus === "available" && updateLatest && (
              <span style={{ display: "inline-flex", alignItems: "center", gap: 4, fontSize: "var(--fs-xs)", color: "var(--success)" }}>
                <Icon name="circle-dot" size={12} />
                发现新版本 v{updateLatest}
              </span>
            )}
            {updateStatus === "up-to-date" && (
              <span style={{ fontSize: "var(--fs-xs)", color: "var(--ink2)" }}>已是最新版本</span>
            )}
            {updateStatus === "error" && updateError && (
              <span style={{ fontSize: "var(--fs-xs)", color: "var(--warn)" }}>{updateError}</span>
            )}
          </div>

          {/* Release notes + install */}
          {updateStatus === "available" && (
            <>
              {updateNotes && (
                <details style={{ marginTop: 8, fontSize: "var(--fs-xs)", color: "var(--ink2)" }}>
                  <summary style={{ cursor: "pointer", userSelect: "none" }}>发布说明</summary>
                  <pre style={{ whiteSpace: "pre-wrap", fontFamily: "var(--mono)", fontSize: "var(--fs-2xs)", color: "var(--ink2)", background: "var(--bg)", border: "1px solid var(--rule)", borderRadius: 6, padding: 8, marginTop: 6, maxHeight: 140, overflowY: "auto" }}>
                    {updateNotes}
                  </pre>
                </details>
              )}
              <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", marginTop: 8 }}>
                <button
                  onClick={() => downloadAndInstall()}
                  disabled={!updateInstallable || updateDownloading}
                  style={{
                    display: "flex", alignItems: "center", gap: 4,
                    fontFamily: "var(--pixel)",
                    fontSize: "var(--fs-2xs)",
                    padding: "6px 14px",
                    border: `1px solid ${updateInstallable ? "var(--brand)" : "var(--rule2)"}`,
                    color: updateInstallable ? "var(--brand)" : "var(--ink2)",
                    background: updateInstallable ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                    borderRadius: 6,
                    cursor: updateInstallable && !updateDownloading ? "pointer" : "default",
                  }}
                >
                  <Icon name="download" size={12} />
                  {updateDownloading ? "下载中…" : "下载并安装"}
                </button>
                {!updateInstallable && (
                  <span style={{ fontSize: "var(--fs-2xs)", color: "var(--warn)" }}>
                    开发构建不支持自动安装
                  </span>
                )}
              </div>
              {updateDownloading && updateProgress != null && (
                <div style={{ marginTop: 10 }}>
                  <div style={{ height: 6, background: "var(--rule2)", borderRadius: 3, overflow: "hidden" }}>
                    <div
                      style={{
                        height: "100%",
                        width: `${Math.round(updateProgress * 100)}%`,
                        background: "var(--brand)",
                        transition: "width 0.2s ease",
                      }}
                    />
                  </div>
                  <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginTop: 4, fontFamily: "var(--mono)" }}>
                    {Math.round(updateProgress * 100)}%
                  </div>
                </div>
              )}
              <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginTop: 8 }}>
                安装完成后应用将自动重启，正在运行的会话会保留在侧栏，重启后可继续或恢复。
              </div>
            </>
          )}

          {/* Startup auto-check toggle */}
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 12, borderTop: "1px solid var(--rule)", paddingTop: 10 }}>
            <span style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>启动时自动检查更新</span>
            <button
              onClick={() => setAutoCheckUpdate(!autoCheckUpdate)}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "4px 12px",
                border: `1px solid ${autoCheckUpdate ? "var(--brand)" : "var(--rule2)"}`,
                color: autoCheckUpdate ? "var(--brand)" : "var(--ink2)",
                background: autoCheckUpdate ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                borderRadius: 6,
                cursor: "pointer",
              }}
            >
              {autoCheckUpdate ? "已开启" : "已关闭"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
