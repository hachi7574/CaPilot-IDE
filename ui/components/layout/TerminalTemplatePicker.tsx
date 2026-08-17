import { useLayoutEffect, useRef, useState } from "react";
import { useStore, TermTemplate } from "../../state/store";
import { spawnTerminal } from "../../state/agentActions";
import { Icon, runtimeIcon } from "../Icon";

/**
 * New-terminal template picker for the project "+" / tab-bar "+" buttons.
 *
 * bash (fixed, always first) / installed agent CLIs (Claude / Codex / dsh / Pi)
 * / user-defined quick-start commands. Agent templates are hidden when their
 * runtime is not detected (same source as Settings → 已安装). Right-click a
 * non-fixed template to rename it or edit its launch command; "＋ 添加快速启动"
 * adds a new one (persisted to localStorage).
 */
export function TerminalTemplatePicker({
  project,
  anchor,
  onClose,
}: {
  /** Project to spawn the terminal under. */
  project: string;
  /** Fixed-position anchor for the dropdown menu. */
  anchor: { x: number; y: number };
  onClose: () => void;
}) {
  const termTemplates = useStore((s) => s.termTemplates);
  const runtimes = useStore((s) => s.runtimes);
  const addTermTemplate = useStore((s) => s.addTermTemplate);
  const updateTermTemplate = useStore((s) => s.updateTermTemplate);
  const removeTermTemplate = useStore((s) => s.removeTermTemplate);
  const [edit, setEdit] = useState<TermTemplate | null>(null);
  const [adding, setAdding] = useState(false);

// Hide agent templates whose CLI isn't installed. Shell templates (bash /
  // bash-rc, including user quick-starts) always stay. While the runtime probe
  // hasn't returned yet, keep agent entries visible to avoid a bash-only flash.
  const availableRuntimeIds = new Set(
    runtimes.filter((r) => r.available).map((r) => r.id)
  );
  const visibleTemplates = termTemplates.filter((t) => {
    if (t.fixed || t.runtime === "bash" || t.runtime === "bash-rc") return true;
    if (runtimes.length === 0) return true;
    return availableRuntimeIds.has(t.runtime);
  });
  // bash / bash-rc share one adapter probe. When missing (common on Windows
  // without Git Bash on PATH), keep the row clickable so spawn still surfaces
  // the install hint toast — but mark it so the user knows before clicking.
  const bashAvailable = (() => {
    const hit = runtimes.find(
      (rt) => rt.id === "bash-rc" || rt.id === "bash" || rt.id.startsWith("bash")
    );
    // Unknown (runtimes not loaded yet) → don't grey out.
    return hit ? hit.available : true;
  })();

  // Keep the menu fully on-screen: the anchor is the "＋" button's bottom-right
  // corner, which can sit close to the viewport edge. Measure after paint and
  // flip right→left / bottom→top when the menu would be clipped.
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number }>({
    left: anchor.x,
    top: anchor.y,
  });
  useLayoutEffect(() => {
    const el = menuRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const pad = 8;
    let left = anchor.x;
    let top = anchor.y;
    if (left + rect.width + pad > window.innerWidth) {
      left = Math.max(pad, window.innerWidth - rect.width - pad);
    }
    if (top + rect.height + pad > window.innerHeight) {
      top = Math.max(pad, window.innerHeight - rect.height - pad);
    }
    setPos({ left, top });
    // Re-measure when the filtered list shrinks (runtime probe resolves mid-open).
  }, [anchor.x, anchor.y, visibleTemplates.length]);

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
        ref={menuRef}
        className="tt-menu"
        style={{ left: pos.left, top: pos.top }}
        onClick={(e) => e.stopPropagation()}
        onContextMenu={(e) => e.stopPropagation()}
      >
        <div className="tt-label">新建终端</div>
        {visibleTemplates.map((t) => {
          const isBash = t.runtime.startsWith("bash");
          const missing = isBash && !bashAvailable;
          return (
            <div
              key={t.id}
              className={"tt-item" + (missing ? " is-missing" : "")}
              onClick={() => {
                // Still attempt spawn when missing — backend returns a
                // Chinese install hint (Git for Windows / PATH) via toast.
                spawnTerminal(project, t).catch(console.error);
                onClose();
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                if (t.fixed) return;
                setEdit(t);
              }}
              title={
                missing
                  ? "未检测到 bash。Windows 请安装 Git for Windows（Git Bash）并重启 CaPilot"
                  : t.fixed
                    ? "固定模板"
                    : "右键编辑 / 重命名"
              }
            >
              <span className="tt-icon">
                <Icon name={runtimeIcon(t.runtime)} size={16} />
              </span>
              <span className="tt-name">{t.name}</span>
              {missing && <span className="tt-cmd">未安装</span>}
              {!missing && t.command && (
                <span className="tt-cmd">{t.command}</span>
              )}
            </div>
          );
        })}
        <div className="tt-sep" />
        <div className="tt-item tt-add" onClick={() => setAdding(true)}>
          <Icon name="plus" size={12} /> 添加快速启动
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
