import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore, TermTemplate } from "../../state/store";
import { spawnTerminal } from "../../state/agentActions";
import {
  ProviderInfo,
  createStructuredAgent,
} from "../../state/structuredAgent";
import { Icon, runtimeIcon } from "../Icon";

/**
 * New-terminal/agent picker for the project "+" / tab-bar "+" buttons.
 *
 * Phase 5: new sessions default to the structured backend. The picker lists
 * every structured provider registered in the daemon (`agent_provider_list`,
 * each with its real backend kind — acp/direct — never hardcoded) as an Agent
 * entry; bash + user-defined quick-start commands stay PTY terminals. The
 * legacy `claude` PTY template remains as an explicitly-marked EOL entry
 * (Claude has no structured provider registered yet, §18.1 pending).
 * Right-click a non-fixed template to rename it or edit its launch command;
 * "＋ 添加快速启动" adds a new one (persisted to localStorage).
 */
export function TerminalTemplatePicker({
  project,
  anchor,
  onClose,
}: {
  /** Project to spawn the session under. */
  project: string;
  /** Fixed-position anchor for the dropdown menu. */
  anchor: { x: number; y: number };
  onClose: () => void;
}) {
  const termTemplates = useStore((s) => s.termTemplates);
  const addTermTemplate = useStore((s) => s.addTermTemplate);
  const updateTermTemplate = useStore((s) => s.updateTermTemplate);
  const removeTermTemplate = useStore((s) => s.removeTermTemplate);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [edit, setEdit] = useState<TermTemplate | null>(null);
  const [adding, setAdding] = useState(false);

  // Structured providers are the daemon's authority (backend kind included).
  useEffect(() => {
    let cancelled = false;
    invoke<ProviderInfo[]>("agent_provider_list")
      .then((list) => {
        if (!cancelled) setProviders(list ?? []);
      })
      .catch(() => {
        // Backend not ready — the provider section stays empty.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Fixed claude template = legacy PTY EOL entry; codex/opencode are now
  // structured providers (their templates would create read-only EOL sessions),
  // so only bash-family templates render as terminals.
  const legacyAgentTemplates = termTemplates.filter((t) => t.runtime === "claude");
  const terminalTemplates = termTemplates.filter(
    (t) => t.runtime === "bash" || t.runtime === "bash-rc"
  );

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
        <div className="tt-label">新建 Agent</div>
        {providers.map((p) => (
          <div
            key={p.provider_id}
            className="tt-item"
            onClick={() => {
              createStructuredAgent({ project, providerId: p.provider_id }).catch(
                (e) => console.error("create structured agent failed", e)
              );
              onClose();
            }}
            title={`启动 ${p.provider_id} 结构化 Agent 会话（${p.backend_kind} 后端）`}
          >
            <span className="tt-icon">
              <Icon name="bot" size={16} />
            </span>
            <span className="tt-name">{p.provider_id}</span>
            <span className="tt-badge">{p.backend_kind === "direct" ? "Direct" : "ACP"}</span>
          </div>
        ))}
        {providers.length === 0 && (
          <div className="tt-item tt-disabled" title="暂无已注册的结构化提供方">
            <span className="tt-icon">
              <Icon name="bot" size={16} />
            </span>
            <span className="tt-name">（无可用提供方）</span>
          </div>
        )}
        <div className="tt-sep" />
        <div className="tt-label">终端</div>
        {terminalTemplates.map((t) => (
          <div
            key={t.id}
            className="tt-item"
            onClick={() => {
              spawnTerminal(project, t).catch(console.error);
              onClose();
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              if (t.fixed) return;
              setEdit(t);
            }}
            title={t.fixed ? "固定模板" : "右键编辑 / 重命名"}
          >
            <span className="tt-icon">
              <Icon name={runtimeIcon(t.runtime)} size={16} />
            </span>
            <span className="tt-name">{t.name}</span>
            {t.command && <span className="tt-cmd">{t.command}</span>}
          </div>
        ))}
        {legacyAgentTemplates.map((t) => (
          <div
            key={t.id}
            className="tt-item tt-legacy"
            onClick={() => {
              spawnTerminal(project, t).catch(console.error);
              onClose();
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              if (t.fixed) return;
              setEdit(t);
            }}
            title="旧版 PTY Agent（EOL，只读兼容入口）"
          >
            <span className="tt-icon">
              <Icon name={runtimeIcon(t.runtime)} size={16} />
            </span>
            <span className="tt-name">{t.name}</span>
            <span className="tt-badge tt-badge-eol">旧版 EOL</span>
          </div>
        ))}
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
