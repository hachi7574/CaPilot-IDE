import { useStore } from "../../state/store";

export function StatusBar() {
  const agents = useStore((s) => s.agents);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);

  const workerCount = [...agents.values()].filter(
    (a) => a.role === "worker"
  ).length;

  return (
    <div className="statusbar">
      <span className="sb-item">
        <span className="sb-dot bt" />
        蓝牙
      </span>
      <span className="sb-item">📶 WiFi</span>
      <span className="sb-sep" />
      <span className="sb-item sb-battery">🔋 —</span>
      <span className="sb-sep" />
      <span className="sb-item">汇报🔔 开</span>
      <span className="sb-item">worker×{workerCount}</span>
      <span className="sb-spacer" />
      <span className="sb-item sb-mode">模式 {permissionMode}</span>
      <span className="sb-item">速度 {speed}</span>
    </div>
  );
}
