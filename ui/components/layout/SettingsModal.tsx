import { useEffect, useRef, useState, type UIEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open } from "@tauri-apps/plugin-dialog";
import {
  useStore,
  FontScale,
  RuntimeInfo,
  UsageConfig,
  WallpaperMode,
} from "../../state/store";
import { THEMES, getTheme, DEFAULT_THEME_ID } from "../../state/themes";
import { checkForUpdate, downloadAndInstall } from "../../state/update";
import { Icon, runtimeIcon } from "../Icon";
import { isShellRuntime, isWindowsHost } from "../../state/shellPath";

interface SettingsModalProps {
  onClose: () => void;
}

/** True when this runtime is an agent CLI (has models / auth / usage). */
function isAgentRuntime(id: string): boolean {
  return !isShellRuntime(id);
}

/** Sort shells first (powershell/cmd/bash/shell), then agents by name. */
function runtimeSortKey(rt: RuntimeInfo): [number, string] {
  switch (rt.id) {
    case "powershell":
      return [0, rt.name];
    case "cmd":
      return [1, rt.name];
    case "shell":
      return [2, rt.name];
    case "bash-rc":
    case "bash":
      return [3, rt.name];
    default:
      return [10, rt.name.toLowerCase()];
  }
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
  shell: { command: "shell", args: "" },
  powershell: { command: "pwsh", args: "-NoLogo" },
  cmd: { command: "cmd.exe", args: "" },
  "bash-rc": { command: "bash", args: "" },
  bash: { command: "bash", args: "--norc" },
};

export function SettingsModal({ onClose }: SettingsModalProps) {
  const runtimes = useStore((s) => s.runtimes);
  const setRuntimes = useStore((s) => s.setRuntimes);
  const setOnboarded = useStore((s) => s.setOnboarded);
  const fontScale = useStore((s) => s.fontScale);
  const setFontScale = useStore((s) => s.setFontScale);
  const themeId = useStore((s) => s.themeId);
  const setThemeId = useStore((s) => s.setThemeId);
  const wallpaperMode = useStore((s) => s.wallpaperMode);
  const setWallpaperMode = useStore((s) => s.setWallpaperMode);
  const wallpaperPath = useStore((s) => s.wallpaperPath);
  const setWallpaperPath = useStore((s) => s.setWallpaperPath);
  const wallpaperOpacity = useStore((s) => s.wallpaperOpacity);
  const setWallpaperOpacity = useStore((s) => s.setWallpaperOpacity);
  const soundEnabled = useStore((s) => s.soundEnabled);
  const setSoundEnabled = useStore((s) => s.setSoundEnabled);
  const ctrlTRuntime = useStore((s) => s.ctrlTRuntime);
  const setCtrlTRuntime = useStore((s) => s.setCtrlTRuntime);
  const [themeMenuOpen, setThemeMenuOpen] = useState(false);
  const themePickerRef = useRef<HTMLDivElement>(null);
  const [runtimeMenuOpen, setRuntimeMenuOpen] = useState(false);
  const runtimePickerRef = useRef<HTMLDivElement>(null);
  const currentTheme = getTheme(themeId) ?? THEMES[0];
  const themeHasWallpaper = Boolean(currentTheme?.wallpaperUrl);
  const wallpaperActive =
    wallpaperMode === "custom"
      ? Boolean(wallpaperPath)
      : wallpaperMode === "auto"
        ? themeHasWallpaper
        : false;
  const wallpaperFileLabel = wallpaperPath
    ? wallpaperPath.replace(/\\/g, "/").split("/").pop() ?? wallpaperPath
    : null;

  const pickWallpaper = async () => {
    try {
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [
          {
            name: "Images",
            extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"],
          },
        ],
      });
      if (typeof selected === "string" && selected) {
        setWallpaperPath(selected);
        setWallpaperMode("custom");
      }
    } catch {
      // dialog cancelled / unavailable
    }
  };
  const [activeSection, setActiveSection] = useState<
    "runtimes" | "appearance" | "sessions" | "updates"
  >("runtimes");

  const jumpToSection = (
    section: "runtimes" | "appearance" | "sessions" | "updates"
  ) => {
    setActiveSection(section);
    document
      .getElementById(`settings-${section}`)
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  const syncSectionFromScroll = (event: UIEvent<HTMLElement>) => {
    const container = event.currentTarget;
    const sectionIds = ["runtimes", "appearance", "sessions", "updates"] as const;
    let visible: (typeof sectionIds)[number] = "runtimes";
    for (const section of sectionIds) {
      const element = document.getElementById(`settings-${section}`);
      if (!element) continue;
      const sectionTop = element.offsetTop - container.offsetTop;
      if (sectionTop <= container.scrollTop + 72) visible = section;
    }
    // The final panel is shorter than the viewport on some window sizes, so it
    // can never reach the 72px threshold. Reaching the scroll bottom still
    // means the update section is the user's current destination.
    if (container.scrollTop + container.clientHeight >= container.scrollHeight - 2) {
      visible = "updates";
    }
    setActiveSection(visible);
  };

  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (themeMenuOpen) {
        setThemeMenuOpen(false);
        return;
      }
      if (runtimeMenuOpen) {
        setRuntimeMenuOpen(false);
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose, themeMenuOpen, runtimeMenuOpen]);

  useEffect(() => {
    if (!themeMenuOpen) return;
    const closeThemeMenu = (event: PointerEvent) => {
      if (!themePickerRef.current?.contains(event.target as Node)) {
        setThemeMenuOpen(false);
      }
    };
    document.addEventListener("pointerdown", closeThemeMenu);
    return () => document.removeEventListener("pointerdown", closeThemeMenu);
  }, [themeMenuOpen]);

  useEffect(() => {
    if (!runtimeMenuOpen) return;
    const closeRuntimeMenu = (event: PointerEvent) => {
      if (!runtimePickerRef.current?.contains(event.target as Node)) {
        setRuntimeMenuOpen(false);
      }
    };
    document.addEventListener("pointerdown", closeRuntimeMenu);
    return () => document.removeEventListener("pointerdown", closeRuntimeMenu);
  }, [runtimeMenuOpen]);

  // App self-update slice.
  const currentVersion = useStore((s) => s.currentVersion);
  const updateStatus = useStore((s) => s.updateStatus);
  const updateLatest = useStore((s) => s.updateLatest);
  const updateNotes = useStore((s) => s.updateNotes);
  const updateError = useStore((s) => s.updateError);
  const updateDownloading = useStore((s) => s.updateDownloading);
  const updateProgress = useStore((s) => s.updateProgress);
  const updateBytesDownloaded = useStore((s) => s.updateBytesDownloaded);
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

  // Opening Settings should never leave the user staring at a blank "no
  // status" row. If we haven't checked this session (or the last check
  // failed), kick one off immediately so 关于与更新 always answers
  // "最新 / 有更新 / 失败".
  useEffect(() => {
    if (updateStatus === "idle" || updateStatus === "error") {
      void checkForUpdate();
    }
    // Only on mount — manual re-check is the button.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [scanning, setScanning] = useState(false);
  const [detectError, setDetectError] = useState<string | null>(null);
  const reDetect = () => {
    setScanning(true);
    setDetectError(null);
    invoke<RuntimeInfo[]>("runtime_list_available")
      .then((runtimes) => {
        setRuntimes(Array.isArray(runtimes) ? runtimes : []);
      })
      .catch((e) => {
        setDetectError(typeof e === "string" ? e : String(e));
      })
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

  // Show shells + agents. On Windows hide the auto `shell` row (users pick
  // PowerShell / CMD / Git Bash explicitly). Show unavailable rows so a
  // failed/slow probe isn't mistaken for "none installed".
  const listedRuntimes = runtimes
    .filter((rt) => {
      if (isWindowsHost() && rt.id === "shell") return false;
      return true;
    })
    .slice()
    .sort((a, b) => {
      const [ak, an] = runtimeSortKey(a);
      const [bk, bn] = runtimeSortKey(b);
      if (ak !== bk) return ak - bk;
      return an.localeCompare(bn);
    });
  const shellRuntimes = listedRuntimes.filter((rt) => isShellRuntime(rt.id));
  const agentRuntimes = listedRuntimes.filter((rt) => isAgentRuntime(rt.id));
  const installedCount = listedRuntimes.filter((rt) => rt.available).length;
  const installedAgents = agentRuntimes.filter((rt) => rt.available);

  return (
    <div className="modal-overlay settings-overlay" onClick={onClose}>
      <div
        className="modal settings-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <header className="modal-header settings-header">
          <div className="settings-title-lockup">
            <span className="settings-kicker">CAPILOT // CONTROL CARTRIDGE</span>
            <h3 id="settings-title">
              <Icon name="settings" size={18} /> 系统设置
            </h3>
          </div>
          <div className="settings-header-meta">
            <span>BUILD {currentVersion ?? "DEV"}</span>
            <button className="modal-close settings-close" onClick={onClose} aria-label="关闭设置">
              <Icon name="x" size={15} />
            </button>
          </div>
        </header>

        <div className="settings-layout">
          <aside className="settings-rail" aria-label="设置分区">
            <div className="settings-rail-label">DIAGNOSTIC BUS</div>
            <nav className="settings-nav">
              <button
                className={activeSection === "runtimes" ? "active" : ""}
                onClick={() => jumpToSection("runtimes")}
              >
                <Icon name="bot" size={14} />
                <span><b>运行时</b><small>Shell 与 Agent</small></span>
              </button>
              <button
                className={activeSection === "appearance" ? "active" : ""}
                onClick={() => jumpToSection("appearance")}
              >
                <Icon name="paintbrush" size={14} />
                <span><b>外观</b><small>主题与字号</small></span>
              </button>
              <button
                className={activeSection === "sessions" ? "active" : ""}
                onClick={() => jumpToSection("sessions")}
              >
                <Icon name="square-terminal" size={14} />
                <span><b>会话</b><small>退出与引导</small></span>
              </button>
              <button
                className={
                  (activeSection === "updates" ? "active" : "") +
                  (updateStatus === "available" ? " has-update" : "")
                }
                onClick={() => jumpToSection("updates")}
              >
                <Icon name="download" size={14} />
                <span>
                  <b>更新</b>
                  <small>
                    {updateStatus === "available" && updateLatest
                      ? `可更新 v${updateLatest}`
                      : "版本与安装"}
                  </small>
                </span>
              </button>
            </nav>
            <div className="settings-rail-readout">
              <span>THEME</span>
              <b>{getTheme(themeId)?.name ?? DEFAULT_THEME_ID}</b>
              <span>AGENTS</span>
              <b>{installedAgents.length.toString().padStart(2, "0")} ONLINE</b>
              <span>SHELLS</span>
              <b>
                {shellRuntimes
                  .filter((r) => r.available)
                  .length.toString()
                  .padStart(2, "0")}{" "}
                READY
              </b>
            </div>
          </aside>

          <main className="settings-content" onScroll={syncSectionFromScroll}>

        {/* Installed runtimes (shells + agents) */}
        <section id="settings-runtimes" className="modal-section settings-panel settings-runtime-panel">
          <div className="settings-section-head">
            <span>RUNTIME BUS</span>
            <h4>运行环境</h4>
            <p>管理系统终端（PowerShell / CMD / Git Bash）与已接入的编码 Agent CLI。</p>
          </div>
          <div className="settings-toolbar">
            <div className="modal-title">
              已检测{" "}
              <span className="settings-count">
                {installedCount}
                {listedRuntimes.length > 0 ? ` / ${listedRuntimes.length}` : ""}
              </span>
            </div>
            <button className="settings-compact-btn" onClick={reDetect} disabled={scanning}>
              <Icon name="refresh-cw" size={11} />
              {scanning ? "检测中…" : "重新检测"}
            </button>
          </div>
          {detectError && (
            <div className="settings-empty-runtime" role="alert">
              <Icon name="triangle-alert" size={18} />
              <span>检测失败</span>
              <small>{detectError}</small>
            </div>
          )}
          {!scanning && !detectError && listedRuntimes.length === 0 && (
            <div className="settings-empty-runtime">
              <Icon name="plug" size={18} />
              <span>未检测到运行时</span>
              <small>安装 CLI / shell 后点「重新检测」。</small>
            </div>
          )}
          {listedRuntimes.map((rt) => {
            const shell = isShellRuntime(rt.id);
            return (
            <div
              key={rt.id}
              className={`settings-runtime${editingId === rt.id ? " expanded" : ""}${
                rt.available ? "" : " is-missing"
              }`}
            >
              <div className="modal-row settings-runtime-row" style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "8px 0", fontSize: "var(--fs-sm)" }}>
                <span className="settings-runtime-name">
                  <Icon name={runtimeIcon(rt.id)} size={14} /> {rt.name}
                  <span
                    className="settings-runtime-version"
                    style={{ opacity: 0.55, marginLeft: 6 }}
                    title={shell ? "系统终端" : "Agent CLI"}
                  >
                    {shell ? "shell" : "agent"}
                  </span>
                  {rt.version && (
                    <span
                      className="settings-runtime-version"
                      title={`${rt.id} ${rt.version}`}
                    >
                      {rt.version.replace(/^v/i, "")}
                    </span>
                  )}
                </span>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span
                    style={{
                      color: rt.available ? "var(--success)" : "var(--ink3)",
                      fontFamily: "var(--mono)",
                      fontSize: "var(--fs-xs)",
                    }}
                  >
                    {rt.available ? (
                      <>
                        <Icon name="check" size={12} style={{ marginRight: 4 }} />
                        {shell
                          ? "可用"
                          : rt.authenticated
                            ? "已登录"
                            : "已安装"}
                      </>
                    ) : (
                      "未检测到"
                    )}
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
            );
          })}
        </section>

        {/* Preferences */}
        <section id="settings-appearance" className="modal-section settings-panel settings-appearance-panel">
          <div className="settings-section-head">
            <span>DISPLAY CARTRIDGES</span>
            <h4>外观与显示</h4>
            <p>选择整套终端材质与语法色，并调整界面信息密度。</p>
          </div>
          <div className="settings-field-label">
            <span>主题风格</span>
            <small>即时应用 · 自动保存</small>
          </div>
          <div className="settings-theme-picker" ref={themePickerRef}>
            <button
              type="button"
              className="settings-theme-trigger"
              aria-label="主题风格"
              aria-haspopup="listbox"
              aria-expanded={themeMenuOpen}
              aria-controls="settings-theme-menu"
              onClick={() => setThemeMenuOpen((open) => !open)}
            >
              <span className="settings-theme-current">
                <b>{currentTheme.name}</b>
                <small>{currentTheme.note}</small>
              </span>
              <span className="settings-theme-palette" aria-label={`${currentTheme.name}配色`} role="img">
                {currentTheme.swatches.map((color, index) => (
                  <i key={`${color}-${index}`} style={{ backgroundColor: color }} title={color} />
                ))}
              </span>
              <span className="settings-theme-chevron" aria-hidden="true" />
            </button>

            {themeMenuOpen && (
              <div className="settings-theme-menu" id="settings-theme-menu" role="listbox" aria-label="主题风格">
                <div className="settings-theme-menu-head">
                  <span>COLOR CARTRIDGES</span>
                  <small>{THEMES.length} 套配色</small>
                </div>
                {THEMES.map((theme) => {
                  const selected = theme.id === themeId;
                  return (
                    <button
                      key={theme.id}
                      type="button"
                      role="option"
                      aria-selected={selected}
                      className={`settings-theme-option${selected ? " active" : ""}`}
                      onClick={() => {
                        setThemeId(theme.id);
                        setThemeMenuOpen(false);
                      }}
                    >
                      <span className="settings-theme-option-palette">
                        <span className="settings-theme-palette" aria-hidden="true">
                          {theme.swatches.map((color, index) => (
                            <i key={`${color}-${index}`} style={{ backgroundColor: color }} />
                          ))}
                        </span>
                        <small>{theme.swatches.join("  ")}</small>
                      </span>
                      <span className="settings-theme-option-copy">
                        <b>{theme.name}</b>
                        <small>{theme.note}</small>
                      </span>
                      <span className="settings-theme-option-state">{selected ? "ACTIVE" : "LOAD"}</span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          <div className="settings-field-label settings-font-label">
            <span>背景图片</span>
            <small>主题内置或自定义本地图片</small>
          </div>
          <div className="settings-wallpaper">
            <div className="settings-segmented" role="radiogroup" aria-label="背景图片来源">
              {(
                [
                  { key: "auto", label: "跟随主题" },
                  { key: "custom", label: "自定义" },
                  { key: "off", label: "关闭" },
                ] as { key: WallpaperMode; label: string }[]
              ).map((opt) => (
                <button
                  key={opt.key}
                  type="button"
                  role="radio"
                  aria-checked={wallpaperMode === opt.key}
                  className={wallpaperMode === opt.key ? "active" : ""}
                  onClick={() => setWallpaperMode(opt.key)}
                >
                  {opt.label}
                </button>
              ))}
            </div>

            <div className="settings-wallpaper-row">
              <span className="settings-wallpaper-status">
                {wallpaperMode === "off" && "背景层已关闭"}
                {wallpaperMode === "auto" &&
                  (themeHasWallpaper
                    ? `使用「${currentTheme.name}」内置背景`
                    : "当前主题没有内置背景")}
                {wallpaperMode === "custom" &&
                  (wallpaperFileLabel
                    ? `自定义：${wallpaperFileLabel}`
                    : "尚未选择图片")}
              </span>
              <div className="settings-wallpaper-actions">
                <button
                  type="button"
                  className="settings-wallpaper-btn"
                  onClick={() => void pickWallpaper()}
                >
                  选择图片
                </button>
                {wallpaperPath && (
                  <button
                    type="button"
                    className="settings-wallpaper-btn ghost"
                    onClick={() => {
                      setWallpaperPath(null);
                      if (wallpaperMode === "custom") setWallpaperMode("auto");
                    }}
                  >
                    清除
                  </button>
                )}
              </div>
            </div>

            <label className={`settings-wallpaper-slider${wallpaperActive ? "" : " disabled"}`}>
              <span>
                透明度
                <b>{Math.round(wallpaperOpacity * 100)}%</b>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                step={1}
                disabled={!wallpaperActive}
                value={Math.round(wallpaperOpacity * 100)}
                onChange={(e) => setWallpaperOpacity(Number(e.target.value) / 100)}
              />
            </label>
          </div>

          <div className="settings-field-label settings-font-label">
            <span>界面字体大小</span>
            <small>终端字号同步调整</small>
          </div>
          <div className="settings-segmented" role="radiogroup" aria-label="界面字体大小">
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
                type="button"
                role="radio"
                aria-checked={fontScale === opt.key}
                className={fontScale === opt.key ? "active" : ""}
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
          <div className="settings-field-label">
            <span>提示音</span>
            <small>Agent 完成一次任务时的提示音</small>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
            <span style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>完成提示音</span>
            <button
              onClick={() => setSoundEnabled(!soundEnabled)}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "4px 12px",
                border: `1px solid ${soundEnabled ? "var(--brand)" : "var(--rule2)"}`,
                color: soundEnabled ? "var(--brand)" : "var(--ink2)",
                background: soundEnabled ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                borderRadius: 6,
                cursor: "pointer",
              }}
            >
              {soundEnabled ? "已开启" : "已关闭"}
            </button>
          </div>
        </section>

        {/* Session end handling */}
        <section id="settings-sessions" className="modal-section settings-panel settings-session-panel">
          <div className="settings-section-head">
            <span>SESSION LIFECYCLE</span>
            <h4>会话行为</h4>
            <p>决定 Agent 自然退出后的保留方式，以及是否重新运行首次引导。</p>
          </div>
          <div className="settings-field-label">
            <span>Ctrl+T 新建终端</span>
            <small>快捷键创建的 Agent 运行时</small>
          </div>
          {(() => {
            const availableRuntimes = runtimes.filter((rt) => rt.available);
            const currentRuntime =
              availableRuntimes.find((rt) => rt.id === ctrlTRuntime) ?? availableRuntimes[0] ?? null;
            return (
              <div className="settings-runtime-picker" ref={runtimePickerRef}>
                <button
                  type="button"
                  className="settings-runtime-trigger"
                  aria-label="Ctrl+T 新建终端运行时"
                  aria-haspopup="listbox"
                  aria-expanded={runtimeMenuOpen}
                  aria-controls="settings-runtime-menu"
                  disabled={availableRuntimes.length === 0}
                  onClick={() => setRuntimeMenuOpen((open) => !open)}
                >
                  <span className="settings-runtime-current">
                    {currentRuntime ? (
                      <>
                        <span className="settings-runtime-current-icon">
                          <Icon name={runtimeIcon(currentRuntime.id)} size={14} />
                        </span>
                        <span className="settings-runtime-current-copy">
                          <b>{currentRuntime.name}</b>
                          <small>{currentRuntime.id}</small>
                        </span>
                      </>
                    ) : (
                      <span className="settings-runtime-current-copy">
                        <b>无可用运行时</b>
                        <small>请先在运行时面板安装</small>
                      </span>
                    )}
                  </span>
                  <span className="settings-theme-chevron" aria-hidden="true" />
                </button>

                {runtimeMenuOpen && availableRuntimes.length > 0 && (
                  <div
                    className="settings-runtime-menu"
                    id="settings-runtime-menu"
                    role="listbox"
                    aria-label="Ctrl+T 新建终端运行时"
                  >
                    {availableRuntimes.map((rt) => {
                      const selected = rt.id === (currentRuntime?.id ?? ctrlTRuntime);
                      return (
                        <button
                          key={rt.id}
                          type="button"
                          role="option"
                          aria-selected={selected}
                          className={`settings-runtime-option${selected ? " active" : ""}`}
                          onClick={() => {
                            setCtrlTRuntime(rt.id);
                            setRuntimeMenuOpen(false);
                          }}
                        >
                          <span className="settings-runtime-option-icon">
                            <Icon name={runtimeIcon(rt.id)} size={14} />
                          </span>
                          <span className="settings-runtime-option-copy">
                            <b>{rt.name}</b>
                            <small>{rt.id}</small>
                          </span>
                          {selected && <span className="settings-runtime-option-state">ACTIVE</span>}
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })()}
          <div className="settings-field-label">
            <span>终端自然退出后</span>
            <small>适用于 exit、任务完成与 Ctrl+D</small>
          </div>
          <div className="settings-choice-grid">
            <button
              className={sessionEndMode === "keep" ? "active" : ""}
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
              <Icon name="history" size={14} />
              <span><b>保留会话</b><small>标记为已结束，仍可从侧栏找回</small></span>
            </button>
            <button
              className={sessionEndMode === "delete" ? "active danger" : "danger"}
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
              <Icon name="trash-2" size={14} />
              <span><b>直接删除</b><small>退出后立即清理会话记录</small></span>
            </button>
          </div>
        </section>

        {/* First-run onboarding */}
        <div className="modal-section settings-panel settings-onboarding-panel">
          <div className="settings-action-copy">
            <Icon name="rocket" size={15} />
            <span><b>首次使用引导</b><small>重新查看运行环境检测与创建会话流程。</small></span>
          </div>
          <div>
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
        <section id="settings-updates" className="modal-section settings-panel settings-update-panel">
          <div className="settings-section-head">
            <span>UPDATES</span>
            <h4>关于与更新</h4>
          </div>

          <div className="settings-update-row">
            <div className="settings-update-identity">
              <b>CaPilot IDE</b>
              <span>当前 v{currentVersion ?? "…"}</span>
              {updateStatus === "available" && updateLatest && (
                <span className="settings-update-badge" role="status">
                  存在可更新版本 v{updateLatest}
                </span>
              )}
            </div>
            <button
              type="button"
              className="settings-update-check"
              onClick={() => checkForUpdate()}
              disabled={updateStatus === "checking" || updateDownloading}
            >
              <Icon name="refresh-cw" size={11} />
              {updateStatus === "checking" ? "检查中…" : "检查更新"}
            </button>
          </div>

          {/* Always-visible verdict card — idle/checking/latest/update/error.
              Before the first check the user still sees an explicit state, not
              a blank panel that looks like "no info". */}
          <div
            className={
              "settings-update-verdict" +
              (updateStatus === "available"
                ? " is-update"
                : updateStatus === "up-to-date"
                  ? " is-latest"
                  : updateStatus === "error"
                    ? " is-warn"
                    : "")
            }
            role="status"
            aria-live="polite"
          >
            {updateStatus === "idle" && (
              <>
                <strong>尚未检查更新</strong>
                <span>打开本页会自动检查；也可点右上角「检查更新」。</span>
              </>
            )}
            {updateStatus === "checking" && (
              <>
                <strong>正在检查更新…</strong>
                <span>
                  当前 v{currentVersion ?? "…"} · 正在连接更新服务器
                </span>
              </>
            )}
            {updateStatus === "up-to-date" && (
              <>
                <strong>已是最新版本</strong>
                <span>
                  当前 v{currentVersion ?? "…"}
                  {updateLatest ? ` · 远端 v${updateLatest}` : ""}
                  {" · 无需更新"}
                </span>
              </>
            )}
            {updateStatus === "available" && updateLatest && (
              <>
                <strong>存在可更新版本</strong>
                <span>
                  当前 v{currentVersion ?? "…"} → 可升级到 v{updateLatest}
                  {" · 点击下方按钮下载并安装"}
                </span>
              </>
            )}
            {updateStatus === "error" && (
              <>
                <strong>检查更新失败</strong>
                <span>{updateError || "未知错误，请稍后重试"}</span>
              </>
            )}
          </div>

          {updateStatus === "available" && (
            <div className="settings-update-actions">
              <div className="settings-update-available-callout">
                已检测到新版本 v{updateLatest}，当前运行的是 v
                {currentVersion ?? "…"}。可立即下载安装。
              </div>
              {updateNotes && (
                <details className="settings-update-notes">
                  <summary>发布说明</summary>
                  <pre>{updateNotes}</pre>
                </details>
              )}
              <div className="settings-update-install-row">
                <button
                  type="button"
                  className="settings-update-install"
                  onClick={() => downloadAndInstall()}
                  disabled={!updateInstallable || updateDownloading}
                >
                  <Icon name="download" size={12} />
                  {updateDownloading
                    ? "下载中…"
                    : updateLatest
                      ? `下载并安装 v${updateLatest}`
                      : "下载并安装"}
                </button>
                {!updateInstallable && (
                  <span className="settings-update-status is-warn">
                    开发构建不支持自动安装，请使用发布包
                  </span>
                )}
              </div>
              {updateDownloading && (
                <div className="settings-update-progress">
                  {updateProgress != null ? (
                    <>
                      <div className="settings-update-progress-track">
                        <div
                          className="settings-update-progress-bar"
                          style={{ width: `${Math.round(updateProgress * 100)}%` }}
                        />
                      </div>
                      <span>{Math.round(updateProgress * 100)}%</span>
                    </>
                  ) : (
                    <span>
                      已下载{" "}
                      {updateBytesDownloaded != null
                        ? `${(updateBytesDownloaded / (1024 * 1024)).toFixed(1)} MB`
                        : "…"}
                    </span>
                  )}
                </div>
              )}
            </div>
          )}

          <div className="settings-update-toggle">
            <span>启动时自动检查更新</span>
            <button
              type="button"
              onClick={() => setAutoCheckUpdate(!autoCheckUpdate)}
              className={autoCheckUpdate ? "active" : ""}
            >
              {autoCheckUpdate ? "已开启" : "已关闭"}
            </button>
          </div>
        </section>
          </main>
        </div>
      </div>
    </div>
  );
}
