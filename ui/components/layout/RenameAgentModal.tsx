import { useEffect, useRef, useState } from "react";
import { renameAgent } from "../../state/agentActions";
import { Icon } from "../Icon";

/** Rename a terminal session. Reused by the tab-bar and sidebar right-click
 *  context menus; the backend persists the new title (DB + `.agent-meta.json`)
 *  and the store updates both the agent record and the tab snapshot. */
export function RenameAgentModal({
  agentId,
  initial,
  onClose,
}: {
  agentId: string;
  initial: string;
  onClose: () => void;
}) {
  const [name, setName] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const submit = async () => {
    const trimmed = name.trim();
    if (!trimmed || busy || trimmed === initial) return;
    setBusy(true);
    setError(null);
    try {
      await renameAgent(agentId, trimmed);
      onClose();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="nproj-overlay" onClick={onClose}>
      <div className="nproj-card" onClick={(e) => e.stopPropagation()}>
        <div className="nproj-title">
          <Icon name="pencil" size={16} /> 重命名终端
        </div>
        <div className="ug-nproj-label">终端名称</div>
        <input
          ref={inputRef}
          className="nproj-input"
          placeholder="终端名称"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        {error && <div className="nproj-error">{error}</div>}
        <div className="nproj-actions">
          <button className="nproj-btn" onClick={onClose}>
            取消
          </button>
          <button
            className="nproj-btn primary"
            onClick={submit}
            disabled={busy || !name.trim() || name.trim() === initial}
          >
            {busy ? "重命名中…" : "重命名"}
          </button>
        </div>
      </div>
    </div>
  );
}
