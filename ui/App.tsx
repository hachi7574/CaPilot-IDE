import { useEffect, useState, type ComponentType } from "react";
import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { Onboarding } from "./components/onboarding/Onboarding";
import { useResourceSync } from "./state/resource";
import { useRuntimeSync } from "./state/runtime";
import { useSessionRestore, useAgentEvents } from "./state/session";
import { useCloneEvents } from "./state/clone";
import { useWorktreeEvents } from "./state/worktree";
import { useUsageSync } from "./state/usage";
import { useContextUsageSync } from "./state/usageContext";
import { useUpdateSync } from "./state/update";
import { useStore } from "./state/store";
import "./App.css";

/**
 * Design-feedback annotation UI is a `tauri dev` tool only.
 *
 * Production builds (`pnpm tauri build` / tagged releases) must not ship the
 * floating tray or pick layer. Vite replaces `import.meta.env.DEV` with the
 * literal `false` at build time, so the dynamic import below is eliminated
 * from the production module graph (a static import would still pull the
 * annotation modules in even behind a dead `&&` branch).
 */
function DevAnnotationsGate() {
  const [Comp, setComp] = useState<ComponentType | null>(null);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    let cancelled = false;
    void import("./components/annotations/DevAnnotations").then((m) => {
      if (!cancelled) setComp(() => m.DevAnnotations);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  if (!import.meta.env.DEV || !Comp) return null;
  return <Comp />;
}

function App() {
  useResourceSync();
  useRuntimeSync();
  useSessionRestore();
  useAgentEvents();
  useCloneEvents();
  useWorktreeEvents();
  useUsageSync();
  useContextUsageSync();
  useUpdateSync();
  const onboarded = useStore((s) => s.onboarded);
  const fontScale = useStore((s) => s.fontScale);
  const themeId = useStore((s) => s.themeId);
  // Reflect the chosen font-size preset on <html> so the CSS `html[data-fs=…]`
  // rules can rescale every `--fs-*` token.
  document.documentElement.dataset.fs = fontScale;
  // Theme tokens live in CSS; reflecting the persisted preset here updates the
  // whole shell (and CodeMirror) without changing component structure.
  document.documentElement.dataset.theme = themeId;
  return (
    <div className="app">
      <div className="app-body">
        <RightSidebar />
        <MainArea />
        <LeftSidebar />
      </div>
      <StatusBar />
      <DevAnnotationsGate />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
