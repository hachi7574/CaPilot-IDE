import { useState } from "react";
import { useStore, TermTemplate } from "../../state/store";
import { spawnTerminal } from "../../state/agentActions";

/**
 * New-terminal template picker for the project "+" / tab-bar "+" buttons.
 *
 * bash (fixed, always first) / claude / user-defined quick-start commands.
 * Right-click a non-fixed template to rename it or edit its launch command;
 * "＋ 添加快速启动" adds a new one (persisted to localStorage).
 */
export function TerminalTemplatePicker({
  project,
  anchor,
  role = "standalone",
  onClose,
}: {
  /** Project to spawn the terminal under ("master" for the tab-bar "+"). */
  project: string;
  /** Fixed-position anchor for the dropdown menu. */
  anchor: { x: number; y: number };
  role?: "master" | "worker" | "standalone";
  onClose: () => void;
}) {
  const termTemplates = useStore((s) => s.termTemplates);
  const addTermTemplate = useStore((s) => s.addTermTemplate);
  const updateTermTemplate = useStore((s) => s.updateTermTemplate);
  const removeTermTemplate = useStore((s) => s.removeTermTemplate);
  const [edit, setEdit] = useState<TermTemplate | null>(null);
  const [adding, setAdding] = useState(false);

  return (
    <>
      <div
        className="tt-backdrop"
        onClick={onClose}
        onContextMenu={(e) => {
          e.preventDefault();
          onClose();
        }}
      />
      <div
        className="tt-menu"
        style={{ left: anchor.x, top: anchor.y }}
        onClick={(e) => e.stopPropagation()}
        onContextMenu={(e) => e.stopPropagation()}
      >
        <div className="tt-label">新建终端</div>
        {termTemplates.map((t) => (
          <div
            key={t.id}
            className="tt-item"
            onClick={() => {
              spawnTerminal(project, t, role).catch(console.error);
              onClose();
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              if (t.fixed) return;
              setEdit(t);
            }}
            title={t.fixed ? "固定模板" : "右键编辑 / 重命名"}
          >
            <span className="tt-icon">{t.runtime.startsWith("bash") ? "🐚" : "🤖"}</span>
            <span className="tt-name">{t.name}</span>
            {t.command && <span className="tt-cmd">{t.command}</span>}
          </div>
        ))}
        <div className="tt-sep" />
        <div className="tt-item tt-add" onClick={() => setAdding(true)}>
          ＋ 添加快速启动
        </div>
      </div>
      {edit && (
        <TermTemplateModal
          title="编辑终端模板"
          name={edit.name}
          command={edit.command}
          canDelete={!edit.fixed}
          onSave={(nm, cmd) => {
            updateTermTemplate(edit.id, { name: nm, command: cmd });
            setEdit(null);
          }}
          onDelete={
            edit.fixed
              ? undefined
              : () => {
                  removeTermTemplate(edit.id);
                  setEdit(null);
                }
          }
          onClose={() => setEdit(null)}
        />
      )}
      {adding && (
        <TermTemplateModal
          title="添加快速启动"
          name=""
          command=""
          canDelete={false}
          onSave={(nm, cmd) => {
            addTermTemplate({
              id: `tpl-${Date.now()}`,
              name: nm,
              command: cmd,
              runtime: "bash-rc",
            });
            setAdding(false);
          }}
          onClose={() => setAdding(false)}
        />
      )}
    </>
  );
}

/** Edit / add modal for a terminal template (name + launch command). */
function TermTemplateModal({
  title,
  name,
  command,
  canDelete,
  onSave,
  onDelete,
  onClose,
}: {
  title: string;
  name: string;
  command: string;
  canDelete: boolean;
  onSave: (name: string, command: string) => void;
  onDelete?: () => void;
  onClose: () => void;
}) {
  const [nm, setNm] = useState(name);
  const [cmd, setCmd] = useState(command);

  const submit = () => {
    const trimmed = nm.trim();
    if (!trimmed) return;
    onSave(trimmed, cmd.trim());
  };

  return (
    <div className="nproj-overlay" onClick={onClose}>
      <div className="nproj-card" onClick={(e) => e.stopPropagation()}>
        <div className="nproj-title">{title}</div>
        <div className="ug-nproj-label">名称</div>
        <input
          className="nproj-input"
          placeholder="终端名称"
          value={nm}
          autoFocus
          onChange={(e) => setNm(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="ug-nproj-label">启动指令</div>
        <input
          className="nproj-input"
          placeholder="在 bash 中执行的命令（可留空）"
          value={cmd}
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="nproj-actions">
          {canDelete && onDelete ? (
            <button className="nproj-btn danger" onClick={onDelete}>
              删除
            </button>
          ) : (
            <button className="nproj-btn" onClick={onClose}>
              取消
            </button>
          )}
          <button
            className="nproj-btn primary"
            onClick={submit}
            disabled={!nm.trim()}
          >
            保存
          </button>
        </div>
      </div>
    </div>
  );
}
