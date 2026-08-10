import { useEffect, useRef, useState } from "react";

interface PermissionConfirmationDialogProps {
  modeLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * IDE-owned confirmation for provider permission presets that remove safety
 * boundaries. Keeping this outside the PTY means both mouse and keyboard users
 * can answer without moving focus into a terminal-only picker.
 */
export function PermissionConfirmationDialog({
  modeLabel,
  onConfirm,
  onCancel,
}: PermissionConfirmationDialogProps) {
  const [confirmFocused, setConfirmFocused] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  const select = (confirm: boolean) => {
    setConfirmFocused(confirm);
    (confirm ? confirmRef : cancelRef).current?.focus();
  };

  return (
    <div
      className="permission-confirm-overlay"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onCancel();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          onCancel();
        } else if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
          event.preventDefault();
          select(!confirmFocused);
        }
      }}
    >
      <section
        className="permission-confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="permission-confirm-title"
      >
        <div id="permission-confirm-title" className="permission-confirm-title">
          启用 {modeLabel}？
        </div>
        <div className="permission-confirm-actions">
          <button
            ref={cancelRef}
            type="button"
            className="permission-confirm-btn"
            onFocus={() => setConfirmFocused(false)}
            onClick={onCancel}
          >
            取消
          </button>
          <button
            ref={confirmRef}
            type="button"
            className="permission-confirm-btn primary"
            onFocus={() => setConfirmFocused(true)}
            onClick={onConfirm}
          >
            启用
          </button>
        </div>
      </section>
    </div>
  );
}
