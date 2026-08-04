import { useStore } from "../../state/store";

export function StatusBar() {
  const agents = useStore((s) => s.agents);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const composerTarget = useStore((s) => s.composerTarget);

  const workerCount = [...agents.values()].filter(
    (a) => a.role === "worker"
  ).length;

  return (
    <div className="statusbar">
      <div className="status-item">
        <span className="status-dot running" />
        <span>workers ×{workerCount}</span>
      </div>
      <div className="status-item">
        <span>mode: [{permissionMode}]</span>
      </div>
      <div className="status-item">
        <span>speed: {speed}</span>
      </div>
      <div className="status-item" style={{ marginLeft: "auto" }}>
        <span>target: {composerTarget}</span>
      </div>
    </div>
  );
}
