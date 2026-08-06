import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { MergeView } from "@codemirror/merge";

interface DiffPanelProps {
  oldText: string;
  newText: string;
}

/**
 * Full-height read-only side-by-side diff (OLD left / NEW right), opened from the
 * Source Control panel. Same @codemirror/merge engine as the inline gutter diff,
 * but fills the whole editor pane.
 */
export function DiffPanel({ oldText, newText }: DiffPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    // Clear any leftover DOM (StrictMode double-mount safety).
    el.textContent = "";
    const readOnlyExt = [
      oneDark,
      lineNumbers(),
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ];
    const view = new MergeView({
      a: { doc: oldText, extensions: readOnlyExt },
      b: { doc: newText, extensions: readOnlyExt },
      parent: el,
      orientation: "a-b",
      gutter: true,
      highlightChanges: true,
    });
    return () => {
      view.destroy();
      if (containerRef.current) containerRef.current.textContent = "";
    };
  }, [oldText, newText]);

  return <div className="diff-panel" ref={containerRef} />;
}
