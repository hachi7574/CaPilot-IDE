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
import { THEMES, getTheme, DEFAULT_THEME_ID, WALLPAPER_IMAGE_EXTS, WALLPAPER_VIDEO_EXTS } from "../../state/themes";
import { checkForUpdate, downloadAndInstall } from "../../state/update";
import {
  loadExitDaemonMode,
  saveExitDaemonMode,
  type ExitDaemonMode,
} from "../../state/exitDaemon";
import { Icon, runtimeIcon } from "../Icon";
import { isShellRuntime, isWindowsHost } from "../../state/shellPath";
import { useT, LOCALES, themeLabel, type Locale } from "../../i18n";

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
  const t = useT();
  const locale = useStore((s) => s.locale);
  const setLocale = useStore((s) => s.setLocale);
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
  const themeLabEnabled = useStore((s) => s.themeLabEnabled);
  const setThemeLabEnabled = useStore((s) => s.setThemeLabEnabled);
  const ctrlTRuntime = useStore((s) => s.ctrlTRuntime);
  const setCtrlTRuntime = useStore((s) => s.setCtrlTRuntime);
  const [themeMenuOpen, setThemeMenuOpen] = useState(false);
  const themePickerRef = useRef<HTMLDivElement>(null);
  const [runtimeMenuOpen, setRuntimeMenuOpen] = useState(false);
  const runtimePickerRef = useRef<HTMLDivElement>(null);
  const [namePacks, setNamePacks] = useState<
    { id: string; name: string; note: string; count: number }[]
  >([]);
  const [namePackId, setNamePackId] = useState("tica-cats");
  const [namePackMenuOpen, setNamePackMenuOpen] = useState(false);
  const namePackPickerRef = useRef<HTMLDivElement>(null);
  const [namePackPasteOpen, setNamePackPasteOpen] = useState(false);
  const [namePackDraft, setNamePackDraft] = useState("");
  const [namePackBusy, setNamePackBusy] = useState(false);
  const [namePackMsg, setNamePackMsg] = useState<string | null>(null);
  const [namePackErr, setNamePackErr] = useState<string | null>(null);
  const currentTheme = getTheme(themeId) ?? THEMES[0];
  const currentThemeLabel =
    themeLabel(locale, currentTheme.id) ?? {
      name: currentTheme.name,
      note: currentTheme.note,
    };
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
            extensions: [...WALLPAPER_IMAGE_EXTS],
          },
          {
            name: "Videos",
            extensions: [...WALLPAPER_VIDEO_EXTS],
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
      if (namePackMenuOpen) {
        setNamePackMenuOpen(false);
        return;
      }
      if (namePackPasteOpen) {
        setNamePackPasteOpen(false);
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose, themeMenuOpen, runtimeMenuOpen, namePackMenuOpen, namePackPasteOpen]);

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

  useEffect(() => {
    let cancelled = false;
    invoke<{
      packs: { id: string; name: string; note: string; count: number }[];
      activeId: string;
    }>("name_packs_list")
      .then((res) => {
        if (cancelled) return;
        setNamePacks(Array.isArray(res.packs) ? res.packs : []);
        if (res.activeId) setNamePackId(res.activeId);
      })
      .catch(() => {
        if (!cancelled) setNamePacks([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!namePackMenuOpen) return;
    const closeMenu = (event: PointerEvent) => {
      if (!namePackPickerRef.current?.contains(event.target as Node)) {
        setNamePackMenuOpen(false);
      }
    };
    document.addEventListener("pointerdown", closeMenu);
    return () => document.removeEventListener("pointerdown", closeMenu);
  }, [namePackMenuOpen]);

  const applyImportedPack = (id: string) => {
    setNamePackId(id);
    setNamePackMenuOpen(false);
    setNamePackPasteOpen(false);
    setNamePackDraft("");
    setNamePackErr(null);
    invoke<{
      packs: { id: string; name: string; note: string; count: number }[];
      activeId: string;
    }>("name_packs_list")
      .then((res) => {
        const packs = Array.isArray(res.packs) ? res.packs : [];
        setNamePacks(packs);
        const active = res.activeId || id;
        setNamePackId(active);
        const pack = packs.find((p) => p.id === active);
        setNamePackMsg(t("settings.namePackImported", { name: pack?.name ?? active }));
      })
      .catch(() => {
        setNamePackMsg(t("settings.namePackImported", { name: id }));
      });
  };

  const importNamePackFile = async () => {
    setNamePackErr(null);
    setNamePackMsg(null);
    try {
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (typeof selected !== "string" || !selected) return;
      setNamePackBusy(true);
      const id = await invoke<string>("name_pack_import", {
        sourcePath: selected,
        content: null,
      });
      applyImportedPack(id);
    } catch (e) {
      setNamePackErr(t("settings.namePackImportFailed", { err: String(e) }));
    } finally {
      setNamePackBusy(false);
    }
  };

  const importNamePackPaste = async () => {
    const text = namePackDraft.trim();
    if (!text) return;
    setNamePackBusy(true);
    setNamePackErr(null);
    setNamePackMsg(null);
    try {
      const id = await invoke<string>("name_pack_import", {
        sourcePath: null,
        content: text,
      });
      applyImportedPack(id);
    } catch (e) {
      setNamePackErr(t("settings.namePackImportFailed", { err: String(e) }));
    } finally {
      setNamePackBusy(false);
    }
  };

  const pickNamePack = (id: string) => {
    setNamePackId(id);
    setNamePackMenuOpen(false);
    invoke("setting_set", { key: "name_pack", value: id }).catch(() => {});
  };

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
  // GUI-exit handling for the PTY daemon / live agent terminals.
  const [exitDaemonMode, setExitDaemonMode] = useState<ExitDaemonMode>("ask");
  useEffect(() => {
    invoke<string | null>("setting_get", { key: "session_end_mode" })
      .then((v) => {
        if (v) setSessionEndMode(v === "delete" ? "delete" : "keep");
      })
      .catch(() => {
        // Backend not ready — keep default.
      });
    loadExitDaemonMode().then(setExitDaemonMode).catch(() => {});
  }, []);

  const changeSessionEndMode = async (mode: "keep" | "delete") => {
    setSessionEndMode(mode);
    await invoke("setting_set", { key: "session_end_mode", value: mode }).catch(
      () => {}
    );
  };

  const changeExitDaemonMode = async (mode: ExitDaemonMode) => {
    setExitDaemonMode(mode);
    await saveExitDaemonMode(mode);
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
              <Icon name="settings" size={18} /> {t("settings.title")}
            </h3>
          </div>
          <div className="settings-header-meta">
            <span>BUILD {currentVersion ?? "DEV"}</span>
            <button className="modal-close settings-close" onClick={onClose} aria-label={t("settings.closeAria")}>
              <Icon name="x" size={15} />
            </button>
          </div>
        </header>

        <div className="settings-layout">
          <aside className="settings-rail" aria-label={t("settings.railAria")}>
            <div className="settings-rail-label">DIAGNOSTIC BUS</div>
            <nav className="settings-nav">
              <button
                className={activeSection === "runtimes" ? "active" : ""}
                onClick={() => jumpToSection("runtimes")}
              >
                <Icon name="bot" size={14} />
                <span><b>{t("settings.navRuntimes")}</b><small>{t("settings.navRuntimesSub")}</small></span>
              </button>
              <button
                className={activeSection === "appearance" ? "active" : ""}
                onClick={() => jumpToSection("appearance")}
              >
                <Icon name="paintbrush" size={14} />
                <span><b>{t("settings.navAppearance")}</b><small>{t("settings.navAppearanceSub")}</small></span>
              </button>
              <button
                className={activeSection === "sessions" ? "active" : ""}
                onClick={() => jumpToSection("sessions")}
              >
                <Icon name="square-terminal" size={14} />
                <span><b>{t("settings.navSessions")}</b><small>{t("settings.navSessionsSub")}</small></span>
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
                  <b>{t("settings.navUpdates")}</b>
                  <small>
                    {updateStatus === "available" && updateLatest
                      ? t("settings.navUpdatesAvailable", { version: updateLatest })
                      : t("settings.navUpdatesSub")}
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
            <h4>{t("settings.runtimeBus")}</h4>
            <p>{t("settings.runtimeDesc")}</p>
          </div>
          <div className="settings-toolbar">
            <div className="modal-title">
              {t("settings.detected")}{" "}
              <span className="settings-count">
                {installedCount}
                {listedRuntimes.length > 0 ? ` / ${listedRuntimes.length}` : ""}
              </span>
            </div>
            <button className="settings-compact-btn" onClick={reDetect} disabled={scanning}>
              <Icon name="refresh-cw" size={11} />
              {scanning ? t("common.scanning") : t("common.redetect")}
            </button>
          </div>
          {detectError && (
            <div className="settings-empty-runtime" role="alert">
              <Icon name="triangle-alert" size={18} />
              <span>{t("settings.detectFailed")}</span>
              <small>{detectError}</small>
            </div>
          )}
          {!scanning && !detectError && listedRuntimes.length === 0 && (
            <div className="settings-empty-runtime">
              <Icon name="plug" size={18} />
              <span>{t("settings.noRuntimes")}</span>
              <small>{t("settings.noRuntimesHint")}</small>
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
                    title={shell ? t("settings.systemTerminal") : t("settings.agentCli")}
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
                          ? t("common.available")
                          : rt.authenticated
                            ? t("common.loggedIn")
                            : t("common.installed")}
                      </>
                    ) : (
                      t("common.notDetected")
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
                    title={t("settings.gearTitle")}
                  >
                    <Icon name="settings" size={12} />
                  </button>
                </div>
              </div>
              {editingId === rt.id && (
                <div style={{ padding: "0 0 10px", display: "flex", flexDirection: "column", gap: 6 }}>
                  <div>
                    <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                      {t("settings.launchCommand")}
                    </div>
                    <input
                      className="modal-text-input"
                      value={editCmd}
                      onChange={(e) => setEditCmd(e.target.value)}
                      onKeyDown={(e) => { if (e.key === "Enter") saveOverride(); }}
                      placeholder={t("settings.launchCmdPlaceholder", { id: rt.id })}
                    />
                  </div>
                  <div>
                    <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                      {t("settings.launchArgs")}
                    </div>
                    <input
                      className="modal-text-input"
                      value={editArgs}
                      onChange={(e) => setEditArgs(e.target.value)}
                      onKeyDown={(e) => { if (e.key === "Enter") saveOverride(); }}
                      placeholder={t("settings.launchArgsPlaceholder")}
                    />
                  </div>
                  <div style={{ display: "flex", gap: 6, marginTop: 2 }}>
                    <button
                      onClick={saveOverride}
                      style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--brand)", color: "var(--brand)", background: "rgb(var(--brand-rgb) / .08)", borderRadius: 6, cursor: "pointer" }}
                    >
                      {t("common.save")}
                    </button>
                    <button
                      onClick={() => setEditingId(null)}
                      style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--rule2)", color: "var(--ink2)", background: "transparent", borderRadius: 6, cursor: "pointer" }}
                    >
                      {t("common.cancel")}
                    </button>
                    {overrides[rt.id] && (
                      <button
                        onClick={() => resetOverride(rt.id)}
                        style={{ fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", padding: "4px 12px", border: "1px solid var(--rule2)", color: "var(--warn)", background: "transparent", borderRadius: 6, cursor: "pointer" }}
                      >
                        {t("common.resetDefault")}
                      </button>
                    )}
                  </div>

                  {/* Rate-limit usage stats (codex/opencode only) */}
                  {(rt.id === "codex" || rt.id === "opencode") && (
                    <div style={{ borderTop: "1px solid var(--rule)", marginTop: 10, paddingTop: 8 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 4, fontFamily: "var(--pixel)", fontSize: "var(--fs-2xs)", color: "var(--ink2)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
                        <Icon name="activity" size={11} /> {t("settings.usageStats")}
                      </div>

                      {/* Enable toggle */}
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8, fontSize: "var(--fs-2xs)", color: "var(--ink2)" }}>
                        <span>{t("settings.usageEnable")}</span>
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
                          {usageEnabled[rt.id] ? t("common.enabled") : t("common.disabled")}
                        </button>
                      </div>

                      {/* opencode: cookie + workspace id */}
                      {rt.id === "opencode" && (
                        <>
                          <div style={{ marginBottom: 6 }}>
                            <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                              {t("settings.opencodeCookie")}
                            </div>
                            <input
                              className="modal-text-input"
                              type="password"
                              value={usageCookie}
                              onChange={(e) => {
                                setUsageCookie(e.target.value);
                                updateUsageConfig("opencode", { auth_cookie: e.target.value });
                              }}
                              placeholder={t("settings.opencodeCookiePh")}
                            />
                          </div>
                          <div style={{ marginBottom: 6 }}>
                            <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 2 }}>
                              {t("settings.workspaceId")}
                            </div>
                            <input
                              className="modal-text-input"
                              value={usageWorkspace}
                              onChange={(e) => {
                                setUsageWorkspace(e.target.value);
                                updateUsageConfig("opencode", { workspace_id: e.target.value });
                              }}
                              placeholder={t("settings.workspaceIdPh")}
                            />
                          </div>
                        </>
                      )}

                      {/* codex: no config (auth auto-discovered) */}
                      {rt.id === "codex" && (
                        <div style={{ fontSize: "var(--fs-2xs)", color: "var(--ink2)", marginBottom: 8 }}>
                          {t("settings.codexAuthHint")}
                          {t("settings.current")}<span style={{ color: rt.authenticated ? "var(--success)" : "var(--warn)" }}>
                            {rt.authenticated ? t("common.loggedIn") : t("common.notLoggedIn")}
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
                          {checkingId === rt.id ? t("common.checking") : t("settings.checkAvailability")}
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
                          {t("settings.statusBarShows")}
                          {usageState[rt.id].windows
                            .map((w) =>
                              `${w.label} ${
                                w.remaining_pct != null
                                  ? t("settings.remaining", {
                                      pct: Math.round(w.remaining_pct),
                                    })
                                  : ""
                              }`.trim()
                            )
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
            <h4>{t("settings.appearanceTitle")}</h4>
            <p>{t("settings.appearanceDesc")}</p>
          </div>

          <div className="settings-field-label">
            <span>{t("language.label")}</span>
            <small>{t("language.hint")}</small>
          </div>
          <div className="settings-segmented" role="radiogroup" aria-label={t("language.label")}>
            {LOCALES.map((opt) => (
              <button
                key={opt.id}
                type="button"
                role="radio"
                aria-checked={locale === opt.id}
                className={locale === opt.id ? "active" : ""}
                onClick={() => setLocale(opt.id as Locale)}
              >
                {opt.nativeLabel}
              </button>
            ))}
          </div>

          <div className="settings-field-label">
            <span>{t("settings.themeStyle")}</span>
            <small>{t("settings.themeInstant")}</small>
          </div>
          <div className="settings-theme-picker" ref={themePickerRef}>
            <button
              type="button"
              className="settings-theme-trigger"
              aria-label={t("settings.themeStyle")}
              aria-haspopup="listbox"
              aria-expanded={themeMenuOpen}
              aria-controls="settings-theme-menu"
              onClick={() => setThemeMenuOpen((open) => !open)}
            >
              <span className="settings-theme-current">
                <b>{currentThemeLabel.name}</b>
                <small>{currentThemeLabel.note}</small>
              </span>
              <span
                className="settings-theme-palette"
                aria-label={t("settings.themePaletteAria", { name: currentThemeLabel.name })}
                role="img"
              >
                {currentTheme.swatches.map((color, index) => (
                  <i key={`${color}-${index}`} style={{ backgroundColor: color }} title={color} />
                ))}
              </span>
              <span className="settings-theme-chevron" aria-hidden="true" />
            </button>

            {themeMenuOpen && (
              <div className="settings-theme-menu" id="settings-theme-menu" role="listbox" aria-label={t("settings.themeStyle")}>
                <div className="settings-theme-menu-head">
                  <span>COLOR CARTRIDGES</span>
                  <small>{t("settings.themeSwatches", { n: THEMES.length })}</small>
                </div>
                {THEMES.map((theme) => {
                  const selected = theme.id === themeId;
                  const label = themeLabel(locale, theme.id) ?? {
                    name: theme.name,
                    note: theme.note,
                  };
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
                        <b>{label.name}</b>
                        <small>{label.note}</small>
                      </span>
                      <span className="settings-theme-option-state">{selected ? "ACTIVE" : "LOAD"}</span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          <div className="settings-field-label settings-font-label">
            <span>{t("settings.wallpaper")}</span>
            <small>{t("settings.wallpaperHint")}</small>
          </div>
          <div className="settings-wallpaper">
            <div className="settings-segmented" role="radiogroup" aria-label={t("settings.wallpaperSource")}>
              {(
                [
                  { key: "auto", label: t("settings.wallpaperAuto") },
                  { key: "custom", label: t("settings.wallpaperCustom") },
                  { key: "off", label: t("settings.wallpaperOff") },
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
                {wallpaperMode === "off" && t("settings.wallpaperOffStatus")}
                {wallpaperMode === "auto" &&
                  (themeHasWallpaper
                    ? t("settings.wallpaperAutoStatus", { name: currentThemeLabel.name })
                    : t("settings.wallpaperAutoNone"))}
                {wallpaperMode === "custom" &&
                  (wallpaperFileLabel
                    ? t("settings.wallpaperCustomStatus", { name: wallpaperFileLabel })
                    : t("settings.wallpaperCustomNone"))}
              </span>
              <div className="settings-wallpaper-actions">
                <button
                  type="button"
                  className="settings-wallpaper-btn"
                  onClick={() => void pickWallpaper()}
                >
                  {t("settings.pickImage")}
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
                    {t("common.clear")}
                  </button>
                )}
              </div>
            </div>

            <label className={`settings-wallpaper-slider${wallpaperActive ? "" : " disabled"}`}>
              <span>
                {t("settings.opacity")}
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
            <span>{t("settings.fontSize")}</span>
            <small>{t("settings.fontSizeHint")}</small>
          </div>
          <div className="settings-segmented" role="radiogroup" aria-label={t("settings.fontSizeAria")}>
            {(
              [
                { key: "s", label: t("settings.fontS") },
                { key: "m", label: t("settings.fontM") },
                { key: "l", label: t("settings.fontL") },
                { key: "xl", label: t("settings.fontXl") },
                { key: "xxl", label: t("settings.fontXxl") },
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
            <span>{t("settings.sound")}</span>
            <small>{t("settings.soundHint")}</small>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
            <span style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>{t("settings.soundToggle")}</span>
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
              {soundEnabled ? t("common.on") : t("common.off")}
            </button>
          </div>

          <div className="settings-field-label">
            <span>{t("settings.themeLab")}</span>
            <small>{t("settings.themeLabHint")}</small>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginTop: 4 }}>
            <span style={{ fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>{t("settings.themeLabToggle")}</span>
            <button
              onClick={() => setThemeLabEnabled(!themeLabEnabled)}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "4px 12px",
                border: `1px solid ${themeLabEnabled ? "var(--brand)" : "var(--rule2)"}`,
                color: themeLabEnabled ? "var(--brand)" : "var(--ink2)",
                background: themeLabEnabled ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                borderRadius: 6,
                cursor: "pointer",
              }}
            >
              {themeLabEnabled ? t("common.on") : t("common.off")}
            </button>
          </div>

          <div className="settings-field-label settings-font-label">
            <span>{t("settings.namePack")}</span>
            <small>{t("settings.namePackHint")}</small>
          </div>
          {(() => {
            const current =
              namePacks.find((p) => p.id === namePackId) ?? namePacks[0] ?? null;
            return (
              <div className="settings-runtime-picker" ref={namePackPickerRef}>
                <button
                  type="button"
                  className="settings-runtime-trigger"
                  aria-label={t("settings.namePack")}
                  aria-haspopup="listbox"
                  aria-expanded={namePackMenuOpen}
                  aria-controls="settings-namepack-menu"
                  disabled={namePacks.length === 0}
                  onClick={() => setNamePackMenuOpen((open) => !open)}
                >
                  <span className="settings-runtime-current">
                    <span className="settings-runtime-current-copy">
                      <b>{current ? current.name : t("settings.namePackEmpty")}</b>
                      <small>
                        {current
                          ? current.note ||
                            t("settings.namePackCount", { n: current.count })
                          : t("settings.namePackEmptyHint")}
                      </small>
                    </span>
                  </span>
                  <span className="settings-theme-chevron" aria-hidden="true" />
                </button>
                {namePackMenuOpen && namePacks.length > 0 && (
                  <div
                    className="settings-runtime-menu"
                    id="settings-namepack-menu"
                    role="listbox"
                    aria-label={t("settings.namePack")}
                  >
                    {namePacks.map((pack) => {
                      const selected = pack.id === (current?.id ?? namePackId);
                      return (
                        <button
                          key={pack.id}
                          type="button"
                          role="option"
                          aria-selected={selected}
                          className={`settings-runtime-option${selected ? " active" : ""}`}
                          onClick={() => pickNamePack(pack.id)}
                        >
                          <span className="settings-runtime-option-copy">
                            <b>{pack.name}</b>
                            <small>
                              {pack.note
                                ? `${pack.note} · ${t("settings.namePackCount", { n: pack.count })}`
                                : t("settings.namePackCount", { n: pack.count })}
                            </small>
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
          <div className="settings-wallpaper-row" style={{ marginTop: 8 }}>
            <div className="settings-wallpaper-actions">
              <button
                type="button"
                className="settings-wallpaper-btn"
                disabled={namePackBusy}
                onClick={() => void importNamePackFile()}
              >
                {t("settings.namePackImportFile")}
              </button>
              <button
                type="button"
                className={`settings-wallpaper-btn${namePackPasteOpen ? "" : " ghost"}`}
                disabled={namePackBusy}
                onClick={() => setNamePackPasteOpen((open) => !open)}
              >
                {t("settings.namePackPaste")}
              </button>
            </div>
          </div>
          {namePackPasteOpen && (
            <div className="settings-namepack-paste">
              <label className="settings-field-label" htmlFor="settings-namepack-draft">
                <span>{t("settings.namePackPasteTitle")}</span>
                <small>{t("settings.namePackPasteHint")}</small>
              </label>
              <textarea
                id="settings-namepack-draft"
                className="modal-text-input"
                rows={7}
                spellCheck={false}
                placeholder={t("settings.namePackPastePh")}
                value={namePackDraft}
                onChange={(e) => setNamePackDraft(e.target.value)}
                style={{ width: "100%", resize: "vertical", fontFamily: "var(--mono)" }}
              />
              <div className="settings-wallpaper-actions" style={{ marginTop: 8 }}>
                <button
                  type="button"
                  className="settings-wallpaper-btn"
                  disabled={namePackBusy || !namePackDraft.trim()}
                  onClick={() => void importNamePackPaste()}
                >
                  {t("settings.namePackSave")}
                </button>
                <button
                  type="button"
                  className="settings-wallpaper-btn ghost"
                  onClick={() => {
                    setNamePackPasteOpen(false);
                    setNamePackDraft("");
                  }}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </div>
          )}
          <p className="settings-help-text">
            <b>{t("settings.namePackFormatTitle")} · </b>
            {t("settings.namePackFormat")}
          </p>
          {namePackMsg && (
            <p className="settings-help-text" style={{ color: "var(--success)" }}>
              {namePackMsg}
            </p>
          )}
          {namePackErr && (
            <p className="settings-help-text" style={{ color: "var(--danger)" }}>
              {namePackErr}
            </p>
          )}
        </section>

        {/* Session end handling */}
        <section id="settings-sessions" className="modal-section settings-panel settings-session-panel">
          <div className="settings-section-head">
            <span>SESSION LIFECYCLE</span>
            <h4>{t("settings.sessionTitle")}</h4>
            <p>{t("settings.sessionDesc")}</p>
          </div>
          <div className="settings-field-label">
            <span>{t("settings.ctrlT")}</span>
            <small>{t("settings.ctrlTHint")}</small>
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
                  aria-label={t("settings.ctrlTAria")}
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
                        <b>{t("settings.noRuntime")}</b>
                        <small>{t("settings.noRuntimeHint")}</small>
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
                    aria-label={t("settings.ctrlTAria")}
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
            <span>{t("settings.sessionEnd")}</span>
            <small>{t("settings.sessionEndHint")}</small>
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
              <span><b>{t("settings.keepSession")}</b><small>{t("settings.keepSessionHint")}</small></span>
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
              <span><b>{t("settings.deleteSession")}</b><small>{t("settings.deleteSessionHint")}</small></span>
            </button>
          </div>
          <div className="settings-field-label" style={{ marginTop: 14 }}>
            <span>{t("settings.exitDaemon")}</span>
            <small>{t("settings.exitDaemonHint")}</small>
          </div>
          <div className="settings-choice-grid">
            <button
              className={exitDaemonMode === "ask" ? "active" : ""}
              onClick={() => void changeExitDaemonMode("ask")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "6px 12px",
                border: `1px solid ${exitDaemonMode === "ask" ? "var(--brand)" : "var(--rule2)"}`,
                color: exitDaemonMode === "ask" ? "var(--brand)" : "var(--ink2)",
                background: exitDaemonMode === "ask" ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                cursor: "pointer",
              }}
            >
              <Icon name="circle-alert" size={14} />
              <span><b>{t("settings.exitDaemonAsk")}</b><small>{t("settings.exitDaemonAskHint")}</small></span>
            </button>
            <button
              className={exitDaemonMode === "keep" ? "active" : ""}
              onClick={() => void changeExitDaemonMode("keep")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "6px 12px",
                border: `1px solid ${exitDaemonMode === "keep" ? "var(--brand)" : "var(--rule2)"}`,
                color: exitDaemonMode === "keep" ? "var(--brand)" : "var(--ink2)",
                background: exitDaemonMode === "keep" ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                cursor: "pointer",
              }}
            >
              <Icon name="play" size={14} />
              <span><b>{t("settings.exitDaemonKeep")}</b><small>{t("settings.exitDaemonKeepHint")}</small></span>
            </button>
            <button
              className={exitDaemonMode === "kill" ? "active danger" : "danger"}
              onClick={() => void changeExitDaemonMode("kill")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: "var(--fs-2xs)",
                padding: "6px 12px",
                border: `1px solid ${exitDaemonMode === "kill" ? "var(--brand)" : "var(--rule2)"}`,
                color: exitDaemonMode === "kill" ? "var(--brand)" : "var(--ink2)",
                background: exitDaemonMode === "kill" ? "rgb(var(--brand-rgb) / .08)" : "transparent",
                cursor: "pointer",
              }}
            >
              <Icon name="square" size={14} />
              <span><b>{t("settings.exitDaemonKill")}</b><small>{t("settings.exitDaemonKillHint")}</small></span>
            </button>
          </div>
        </section>

        {/* First-run onboarding */}
        <div className="modal-section settings-panel settings-onboarding-panel">
          <div className="settings-action-copy">
            <Icon name="rocket" size={15} />
            <span><b>{t("settings.onboarding")}</b><small>{t("settings.onboardingHint")}</small></span>
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
              {t("settings.reshow")}
            </button>
          </div>
        </div>

        {/* About / Updates */}
        <section id="settings-updates" className="modal-section settings-panel settings-update-panel">
          <div className="settings-section-head">
            <span>UPDATES</span>
            <h4>{t("settings.aboutTitle")}</h4>
          </div>

          <div className="settings-update-row">
            <div className="settings-update-identity">
              <b>CaPilot IDE</b>
              <span>{t("settings.currentVersion", { version: currentVersion ?? "…" })}</span>
              {updateStatus === "available" && updateLatest && (
                <span className="settings-update-badge" role="status">
                  {t("settings.updateAvailableBadge", { version: updateLatest })}
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
              {updateStatus === "checking" ? t("common.checking") : t("settings.checkUpdate")}
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
                <strong>{t("settings.notChecked")}</strong>
                <span>{t("settings.notCheckedHint")}</span>
              </>
            )}
            {updateStatus === "checking" && (
              <>
                <strong>{t("settings.checkingUpdate")}</strong>
                <span>
                  {t("settings.checkingUpdateHint", { version: currentVersion ?? "…" })}
                </span>
              </>
            )}
            {updateStatus === "up-to-date" && (
              <>
                <strong>{t("settings.upToDate")}</strong>
                <span>
                  {t("settings.upToDateHint", {
                    version: currentVersion ?? "…",
                    remote: updateLatest ? ` · remote v${updateLatest}` : "",
                  })}
                </span>
              </>
            )}
            {updateStatus === "available" && updateLatest && (
              <>
                <strong>{t("settings.updateAvailable")}</strong>
                <span>
                  {t("settings.updateAvailableHint", {
                    current: currentVersion ?? "…",
                    latest: updateLatest,
                  })}
                </span>
              </>
            )}
            {updateStatus === "error" && (
              <>
                <strong>{t("settings.checkFailed")}</strong>
                <span>{updateError || t("settings.unknownError")}</span>
              </>
            )}
          </div>

          {updateStatus === "available" && (
            <div className="settings-update-actions">
              <div className="settings-update-available-callout">
                {t("settings.updateCallout", {
                  latest: updateLatest,
                  current: currentVersion ?? "…",
                })}
              </div>
              {updateNotes && (
                <details className="settings-update-notes">
                  <summary>{t("settings.releaseNotes")}</summary>
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
                    ? t("settings.downloading")
                    : updateLatest
                      ? t("settings.downloadInstallV", { version: updateLatest })
                      : t("settings.downloadInstall")}
                </button>
                {!updateInstallable && (
                  <span className="settings-update-status is-warn">
                    {t("settings.devNoAutoInstall")}
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
                      {t("settings.downloaded", {
                        size:
                          updateBytesDownloaded != null
                            ? `${(updateBytesDownloaded / (1024 * 1024)).toFixed(1)} MB`
                            : "…",
                      })}
                    </span>
                  )}
                </div>
              )}
            </div>
          )}

          <div className="settings-update-toggle">
            <span>{t("settings.autoCheck")}</span>
            <button
              type="button"
              onClick={() => setAutoCheckUpdate(!autoCheckUpdate)}
              className={autoCheckUpdate ? "active" : ""}
            >
              {autoCheckUpdate ? t("common.on") : t("common.off")}
            </button>
          </div>
        </section>
          </main>
        </div>
      </div>
    </div>
  );
}
