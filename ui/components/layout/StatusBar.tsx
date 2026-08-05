import { useStore } from "../../state/store";
import { connectEsp, disconnectEsp } from "../../state/esp";
import { setSmartReturn } from "../../state/orchestration";

export function StatusBar() {
  const agents = useStore((s) => s.agents);
  const workerInfos = useStore((s) => s.workerInfos);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const smartReturn = useStore((s) => s.smartReturn);
  const espStatus = useStore((s) => s.espStatus);
  const espConnecting = useStore((s) => s.espConnecting);

  const workerCount = Math.max(
    workerInfos.length,
    [...agents.values()].filter((a) => a.role === "worker").length
  );

  const handleEspClick = async () => {
    if (espStatus.connected) {
      await disconnectEsp();
    } else {
      const err = await connectEsp();
      if (err) console.error("ESP connect failed:", err);
    }
  };

  const toggleSmartReturn = () => setSmartReturn(!smartReturn);

  return (
    <div className="statusbar">
      <span
        className="sb-item"
        onClick={handleEspClick}
        style={{ cursor: "pointer" }}
        title={espStatus.connected ? "ESP connected — click to disconnect" : "Click to connect ESP (BLE)"}
      >
        <span className={`sb-dot ${espStatus.connected ? "bt" : ""}`} />
        {espStatus.connected
          ? `${espStatus.kind === "wifi" ? "📶 WiFi" : espStatus.kind === "usb" ? "🔌 USB" : "🔵 BLE"}`
          : espConnecting
          ? "connecting…"
          : "📡 ESP off"}
      </span>
      <span className="sb-item">📶 WiFi</span>
      <span className="sb-sep" />
      <span className={`sb-item sb-battery`}>
        🔋 {espStatus.battery_pct !== null ? `${espStatus.battery_pct}%` : "—"}
      </span>
      <span className="sb-sep" />
      <span
        className={`sb-item${smartReturn ? "" : " off"}`}
        onClick={toggleSmartReturn}
        style={{ cursor: "pointer" }}
        title="master 智能返回（分级汇报）开关"
      >
        汇报🔔 {smartReturn ? "开" : "关"}
      </span>
      <span className="sb-item">worker×{workerCount}</span>
      <span className="sb-spacer" />
      <span className="sb-item sb-mode">模式 {permissionMode}</span>
      <span className="sb-item">速度 {speed}</span>
    </div>
  );
}
