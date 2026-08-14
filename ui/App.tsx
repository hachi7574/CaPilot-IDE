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
import { useStore } from "./state/store";
import { AnnotationLayer } from "./components/annotations/AnnotationLayer";
import { AnnotationTray } from "./components/annotations/AnnotationTray";
import "./App.css";

function App() {
  useResourceSync();
  useRuntimeSync();
  useSessionRestore();
  useAgentEvents();
  useCloneEvents();
  useWorktreeEvents();
  useUsageSync();
  useContextUsageSync();
  const onboarded = useStore((s) => s.onboarded);
  const fontScale = useStore((s) => s.fontScale);
  // Reflect the chosen font-size preset on <html> so the CSS `html[data-fs=…]`
  // rules can rescale every `--fs-*` token.
  document.documentElement.dataset.fs = fontScale;
  return (
    <div className="app">
      <div className="app-body">
        <LeftSidebar />
        <MainArea />
        <RightSidebar />
      </div>
      <StatusBar />
      <AnnotationLayer />
      <AnnotationTray />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
