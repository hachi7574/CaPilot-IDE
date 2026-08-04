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

    const loadFile = async () => {
      let content = "";
      try {
        content = await invoke<string>("fs_read", { path: filePath });
      } catch {
        content = `// Could not read: ${filePath}`;
      }

      const updateListener = EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          // Autosave with debounce
          clearTimeout(debounceRef.current);
          debounceRef.current = setTimeout(() => {
            const text = update.state.doc.toString();
            invoke("fs_write", { path: filePath, content: text }).catch(console.error);
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

      const view = new EditorView({
        state,
        parent: containerRef.current!,
      });

      viewRef.current = view;
    };

    loadFile();

    return () => {
      clearTimeout(debounceRef.current);
      viewRef.current?.destroy();
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
