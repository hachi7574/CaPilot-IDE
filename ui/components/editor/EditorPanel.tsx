import { useEffect, useRef } from "react";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { defaultKeymap } from "@codemirror/commands";
import { oneDark } from "@codemirror/theme-one-dark";
import { javascript } from "@codemirror/lang-javascript";
import { rust } from "@codemirror/lang-rust";
import { python } from "@codemirror/lang-python";
import { invoke } from "@tauri-apps/api/core";

interface EditorPanelProps {
  filePath: string;
}

const LANG_MAP: Record<string, () => any> = {
  rs: rust,
  py: python,
};

function getLangExtension(filePath: string) {
  const ext = filePath.split(".").pop() ?? "";
  const fn = LANG_MAP[ext];
  if (fn) return fn();
  return javascript({ typescript: ext === "ts" || ext === "tsx" });
}

export function EditorPanel({ filePath }: EditorPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => {
    if (!containerRef.current) return;

    // Guard against the async load race: opening file A then file B before A's
    // `fs_read` resolves would otherwise mount two .cm-editor into one container
    // (view_A leaks and autosaves into file A while the user edits B). The
    // cancelled flag is set by cleanup, so a superseded load bails on resolve.
    let cancelled = false;

    const loadFile = async () => {
      let content = "";
      try {
        content = await invoke<string>("fs_read", { path: filePath });
      } catch {
        content = `// Could not read: ${filePath}`;
      }
      if (cancelled) return; // superseded by a newer file / unmount

      const updateListener = EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          // Autosave with debounce. Capture the path this view was created for
          // so a pending timer always writes to the correct file.
          const savedPath = filePath;
          clearTimeout(debounceRef.current);
          debounceRef.current = setTimeout(() => {
            const text = update.state.doc.toString();
            invoke("fs_write", { path: savedPath, content: text }).catch(console.error);
          }, 800);
        }
      });

      const state = EditorState.create({
        doc: content,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          keymap.of(defaultKeymap),
          oneDark,
          getLangExtension(filePath),
          updateListener,
        ],
      });

      // Destroy any existing view before creating a new one so there is never
      // more than one .cm-editor in the container.
      viewRef.current?.destroy();
      const view = new EditorView({
        state,
        parent: containerRef.current!,
      });

      viewRef.current = view;
    };

    loadFile();

    return () => {
      cancelled = true;
      // Flush a pending autosave instead of only clearing the timer, so edits
      // within 800ms of closing the editor aren't lost.
      const pending = debounceRef.current;
      debounceRef.current = undefined;
      if (pending) {
        clearTimeout(pending);
        const view = viewRef.current;
        if (view) {
          const text = view.state.doc.toString();
          invoke("fs_write", { path: filePath, content: text }).catch(console.error);
        }
      }
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, [filePath]);

  return (
    <div
      ref={containerRef}
      style={{
        flex: 1,
        overflow: "auto",
      }}
    />
  );
}
