import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { Onboarding } from "./components/onboarding/Onboarding";
import { useResourceSync } from "./state/resource";
import { useRuntimeSync } from "./state/runtime";
import { useSessionRestore, useAgentEvents } from "./state/session";
import { useAcpEvents } from "./state/acpEvents";
import { useCloneEvents } from "./state/clone";
import { useWorktreeEvents } from "./state/worktree";
import { useUsageSync } from "./state/usage";
import { useContextUsageSync } from "./state/usageContext";
import { useUpdateSync } from "./state/update";
import { useStore } from "./state/store";
import { AnnotationLayer } from "./components/annotations/AnnotationLayer";
import { AnnotationTray } from "./components/annotations/AnnotationTray";
import "./App.css";

function App() {
  useResourceSync();
  useRuntimeSync();
  useSessionRestore();
  useAgentEvents();
  useAcpEvents();
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
      <AnnotationLayer />
      <AnnotationTray />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
