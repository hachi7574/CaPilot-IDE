import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";
import { connectEsp, disconnectEsp } from "../../state/esp";
import { setSmartReturn } from "../../state/orchestration";

interface SettingsModalProps {
  onClose: () => void;
}

/** Persisted preference: system notifications on/off (default on). */
const NOTIFICATIONS_KEY = "capilot.notifications";

const APP_VERSION = "0.1.0";

export function SettingsModal({ onClose }: SettingsModalProps) {
  const runtimes = useStore((s) => s.runtimes);
  const espStatus = useStore((s) => s.espStatus);
  const espConnecting = useStore((s) => s.espConnecting);
  const smartReturn = useStore((s) => s.smartReturn);
  const setSmartReturnState = useStore((s) => s.setSmartReturn);
  const setOnboarded = useStore((s) => s.setOnboarded);

  const [notifEnabled, setNotifEnabled] = useState(() => {
    try {
      return localStorage.getItem(NOTIFICATIONS_KEY) !== "0";
    } catch {
      return true;
    }
  });

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

  const toggleNotifications = () => {
    const next = !notifEnabled;
    setNotifEnabled(next);
    try {
      localStorage.setItem(NOTIFICATIONS_KEY, next ? "1" : "0");
    } catch {
      // ignore storage errors
    }
  };

  const toggleSmart = async () => {
    const next = !smartReturn;
    setSmartReturnState(next);
    await setSmartReturn(next);
  };

  const handleEsp = async () => {
    if (espStatus.connected) {
      await disconnectEsp();
    } else {
      const err = await connectEsp();
      if (err) console.error("ESP connect failed:", err);
    }
  };

  const reShowOnboarding = () => {
    setOnboarded(false);
    onClose();
  };

  return (
    <div
      className="modal-overlay"
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.6)",
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
          <h3 style={{ fontFamily: "var(--pixel-body)", fontSize: 18, color: "var(--brand)", letterSpacing: 1 }}>
            ⚙ Settings
          </h3>
          <button className="modal-close" onClick={onClose} style={{ background: "none", border: "none", color: "var(--muted)", fontSize: 20, cursor: "pointer" }}>
            ×
          </button>
        </div>

        {/* Runtime management */}
        <div className="modal-section" style={{ marginBottom: 20 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            Runtime 管理
          </div>
          {runtimes.length === 0 && (
            <div style={{ fontSize: 12, color: "var(--muted)" }}>No runtimes detected</div>
          )}
          {runtimes.map((rt) => (
            <div key={rt.id} className="modal-row" style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", borderBottom: "1px solid var(--rule)", fontSize: 12 }}>
              <span style={{ color: "var(--ink2)" }}>{rt.name}</span>
              <span style={{ color: rt.available ? "var(--success)" : "var(--danger)", fontFamily: "var(--mono)", fontSize: 11 }}>
                {rt.available ? (rt.authenticated ? "✓ 已登录" : "✓ 已安装") : "✕ 未安装"}
              </span>
            </div>
          ))}
        </div>

        {/* ESP pairing */}
        <div className="modal-section" style={{ marginBottom: 20 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            ESP 设备
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", fontSize: 12 }}>
            <span style={{ color: "var(--ink2)" }}>
              {espStatus.connected ? (espStatus.name ?? "ESP32-C5") : "未连接"}
            </span>
            <button
              onClick={handleEsp}
              className="modal-btn"
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
                padding: "5px 12px",
                border: "1px solid var(--brand)",
                color: "var(--brand)",
                background: espStatus.connected ? "rgba(139,92,246,.08)" : "transparent",
                cursor: "pointer",
              }}
            >
              {espConnecting ? "连接中…" : espStatus.connected ? "断开" : "连接 (BLE)"}
            </button>
          </div>
          {espStatus.connected && (
            <div style={{ fontSize: 11, color: "var(--muted)", fontFamily: "var(--mono)", marginTop: 4 }}>
              {espStatus.address}
              {espStatus.battery_pct !== null && ` · 🔋 ${espStatus.battery_pct}%`}
            </div>
          )}
        </div>

        {/* Preferences */}
        <div className="modal-section">
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            通用偏好
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", fontSize: 12 }}>
            <span style={{ color: "var(--ink2)" }}>Master 智能返回</span>
            <button
              onClick={toggleSmart}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
                padding: "4px 10px",
                border: `1px solid ${smartReturn ? "var(--success)" : "var(--rule2)"}`,
                color: smartReturn ? "var(--success)" : "var(--muted)",
                background: smartReturn ? "rgba(74,222,128,.08)" : "transparent",
                cursor: "pointer",
              }}
            >
              {smartReturn ? "开" : "关"}
            </button>
          </div>
        </div>

        {/* Session end handling */}
        <div className="modal-section" style={{ marginBottom: 20 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            会话
          </div>
          <div style={{ fontSize: 11, color: "var(--muted)", marginBottom: 8 }}>
            进程自然退出后（终端里 exit / 任务跑完 / Ctrl+D，不是点 × 关闭）
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              onClick={() => changeSessionEndMode("keep")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
                padding: "6px 12px",
                border: `1px solid ${sessionEndMode === "keep" ? "var(--brand)" : "var(--rule2)"}`,
                color: sessionEndMode === "keep" ? "var(--brand)" : "var(--muted)",
                background: sessionEndMode === "keep" ? "rgba(139,92,246,.08)" : "transparent",
                cursor: "pointer",
              }}
            >
              保留并标记已结束（侧栏可找回）
            </button>
            <button
              onClick={() => changeSessionEndMode("delete")}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
                padding: "6px 12px",
                border: `1px solid ${sessionEndMode === "delete" ? "var(--brand)" : "var(--rule2)"}`,
                color: sessionEndMode === "delete" ? "var(--brand)" : "var(--muted)",
                background: sessionEndMode === "delete" ? "rgba(139,92,246,.08)" : "transparent",
                cursor: "pointer",
              }}
            >
              直接删除
            </button>
          </div>
        </div>

        {/* Notifications */}
        <div className="modal-section" style={{ marginBottom: 20 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            通知
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", fontSize: 12 }}>
            <span style={{ color: "var(--ink2)" }}>系统通知（Worker 完成 / ESP 断连）</span>
            <button
              onClick={toggleNotifications}
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
                padding: "4px 10px",
                border: `1px solid ${notifEnabled ? "var(--success)" : "var(--rule2)"}`,
                color: notifEnabled ? "var(--success)" : "var(--muted)",
                background: notifEnabled ? "rgba(74,222,128,.08)" : "transparent",
                cursor: "pointer",
              }}
            >
              {notifEnabled ? "开" : "关"}
            </button>
          </div>
        </div>

        {/* First-run onboarding */}
        <div className="modal-section" style={{ marginBottom: 20 }}>
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            首次引导
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "6px 0", fontSize: 12 }}>
            <span style={{ color: "var(--ink2)" }}>重新显示引导流程</span>
            <button
              onClick={reShowOnboarding}
              className="modal-btn"
              style={{
                fontFamily: "var(--pixel)",
                fontSize: 10,
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

        {/* About */}
        <div className="modal-section">
          <div className="modal-title" style={{ fontFamily: "var(--pixel)", fontSize: 10, color: "var(--muted)", letterSpacing: 1, textTransform: "uppercase", marginBottom: 8 }}>
            关于
          </div>
          <div style={{ fontSize: 12, color: "var(--ink2)" }}>
            CaPilot IDE <span style={{ fontFamily: "var(--mono)", color: "var(--muted)" }}>v{APP_VERSION}</span>
          </div>
          <div style={{ fontSize: 11, color: "var(--muted)", marginTop: 4, fontFamily: "var(--mono)" }}>
            AI Agent Orchestration Workbench · Master/Worker
          </div>
        </div>
      </div>
    </div>
  );
}
