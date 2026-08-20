import { useLayoutEffect, useRef, useState } from "react";
import { useStore, TermTemplate } from "../../state/store";
import { spawnTerminal } from "../../state/agentActions";
import {
  detectShellFlavor,
  isShellRuntime,
  isWindowsHost,
} from "../../state/shellPath";
import { Icon, runtimeIcon } from "../Icon";
import { useT } from "../../i18n";

/**
 * New-terminal template picker for the project "+" / tab-bar "+" buttons.
 *
 * On Windows: PowerShell / CMD / Git Bash (when installed) / agent CLIs /
 * user-defined quick-start commands. On Unix: OS shell / bash / agents /
 * quick-starts. Agent templates are hidden when their runtime is not detected
 * (same source as Settings → 已安装). Right-click a non-fixed template to
 * rename it or edit its launch command.
 *
 * Quick-start commands are typed into the chosen shell after it reaches its
 * prompt — so the command line must match that shell (PowerShell / cmd on
 * Windows, $SHELL on Unix). See the modal hint below.
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
  const t = useT();
  const termTemplates = useStore((s) => s.termTemplates);
  const runtimes = useStore((s) => s.runtimes);
  const enabledRuntimes = useStore((s) => s.enabledRuntimes);
  const addTermTemplate = useStore((s) => s.addTermTemplate);
  const updateTermTemplate = useStore((s) => s.updateTermTemplate);
  const removeTermTemplate = useStore((s) => s.removeTermTemplate);
  const [edit, setEdit] = useState<TermTemplate | null>(null);
  const [adding, setAdding] = useState(false);

  const runtimeAvailable = (id: string): boolean => {
    const hit = runtimes.find((rt) => rt.id === id);
    // Unknown (runtimes not loaded yet) → keep the row (avoid flash).
    return hit ? hit.available : true;
  };
  const runtimeEnabled = (id: string): boolean => {
    if (enabledRuntimes === null) return true;
    return enabledRuntimes.includes(id);
  };
  // Fixed OS shell always stays. User quick-starts stay.
  // Optional shells (bash / powershell / cmd) hide when missing.
  // Agent rows hide unless detected AND enabled in Settings.
  const visibleTemplates = termTemplates.filter((tpl) => {
    if (tpl.fixed) return true;
    // User quick-starts with a command always show (they target a shell).
    if (tpl.command && isShellRuntime(tpl.runtime)) return true;
    if (runtimes.length === 0) return true;
    if (!runtimeAvailable(tpl.runtime)) return false;
    return runtimeEnabled(tpl.runtime);
  });

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

  /** Default runtime for new quick-starts. */
  const quickStartRuntime = (): TermTemplate["runtime"] => {
    if (isWindowsHost()) {
      if (runtimeAvailable("powershell")) return "powershell";
      if (runtimeAvailable("cmd")) return "cmd";
      return "powershell";
    }
    return "shell";
  };

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
        <div className="tt-label">{t("terminalPicker.title")}</div>
        {visibleTemplates.map((tpl) => {
          return (
            <div
              key={tpl.id}
              className="tt-item"
              onClick={() => {
                spawnTerminal(project, tpl).catch(console.error);
                onClose();
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                if (tpl.fixed) return;
                setEdit(tpl);
              }}
              title={
                tpl.fixed
                  ? t("terminalPicker.fixedTitle")
                  : t("terminalPicker.editTitle")
              }
            >
              <span className="tt-icon">
                <Icon name={runtimeIcon(tpl.runtime)} size={16} />
              </span>
              <span className="tt-name">
                {tpl.fixed && (tpl.id === "shell" || tpl.name === "终端")
                  ? t("common.terminal")
                  : tpl.name}
              </span>
              {tpl.command && <span className="tt-cmd">{tpl.command}</span>}
            </div>
          );
        })}
        <div className="tt-sep" />
        <div className="tt-item tt-add" onClick={() => setAdding(true)}>
          <Icon name="plus" size={12} /> {t("terminalPicker.addQuickStart")}
        </div>
      </div>
      {edit && (
        <TermTemplateModal
          title={t("terminalPicker.editTemplate")}
          name={edit.name}
          command={edit.command}
          runtime={edit.runtime}
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
          title={t("terminalPicker.addQuickStart")}
          name=""
          command=""
          runtime={quickStartRuntime()}
          canDelete={false}
          onSave={(nm, cmd) => {
            addTermTemplate({
              id: `tpl-${Date.now()}`,
              name: nm,
              command: cmd,
              // Quick-starts run inside the preferred OS shell.
              runtime: quickStartRuntime(),
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
  runtime,
  canDelete,
  onSave,
  onDelete,
  onClose,
}: {
  title: string;
  name: string;
  command: string;
  runtime: string;
  canDelete: boolean;
  onSave: (name: string, command: string) => void;
  onDelete?: () => void;
  onClose: () => void;
}) {
  const t = useT();
  const [nm, setNm] = useState(name);
  const [cmd, setCmd] = useState(command);
  const shellRt = useStore((s) => s.runtimes.find((r) => r.id === runtime));
  const flavor = detectShellFlavor(runtime, shellRt?.name);
  const shellHint = (() => {
    if (flavor === "powershell") {
      return t("terminalPicker.hintPs");
    }
    if (flavor === "cmd") {
      return t("terminalPicker.hintCmd");
    }
    if (runtime === "bash-rc" || runtime.startsWith("bash")) {
      return isWindowsHost()
        ? t("terminalPicker.hintGitBash")
        : t("terminalPicker.hintBash");
    }
    if (isWindowsHost()) {
      return t("terminalPicker.hintWin");
    }
    return t("terminalPicker.hintShell");
  })();
  const cmdPlaceholder =
    flavor === "powershell"
      ? t("terminalPicker.cmdPhPs")
      : flavor === "cmd"
        ? t("terminalPicker.cmdPhCmd")
        : t("terminalPicker.cmdPh");

  const submit = () => {
    const trimmed = nm.trim();
    if (!trimmed) return;
    onSave(trimmed, cmd.trim());
  };

  return (
    <div className="nproj-overlay" onClick={onClose}>
      <div className="nproj-card" onClick={(e) => e.stopPropagation()}>
        <div className="nproj-title">{title}</div>
        <div className="ug-nproj-label">{t("terminalPicker.templateName")}</div>
        <input
          className="nproj-input"
          placeholder={t("terminalPicker.namePh")}
          value={nm}
          autoFocus
          onChange={(e) => setNm(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="ug-nproj-label">{t("terminalPicker.templateCommand")}</div>
        <input
          className="nproj-input"
          placeholder={cmdPlaceholder}
          value={cmd}
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        <div
          className="ug-nproj-label"
          style={{ opacity: 0.7, fontWeight: 400, marginTop: 4, lineHeight: 1.4 }}
        >
          {shellHint}
        </div>
        <div className="nproj-actions">
          {canDelete && onDelete ? (
            <button className="nproj-btn danger" onClick={onDelete}>
              {t("common.delete")}
            </button>
          ) : (
            <button className="nproj-btn" onClick={onClose}>
              {t("common.cancel")}
            </button>
          )}
          <button
            className="nproj-btn primary"
            onClick={submit}
            disabled={!nm.trim()}
          >
            {t("common.save")}
          </button>
        </div>
      </div>
    </div>
  );
}
