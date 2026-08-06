import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { Onboarding } from "./components/onboarding/Onboarding";
import { useEspSync } from "./state/esp";
import { useOrchestrationSync } from "./state/orchestration";
import { useResourceSync } from "./state/resource";
import { useSessionRestore, useAgentEvents } from "./state/session";
import { useNotifications } from "./state/notifications";
import { useStore } from "./state/store";
import "./App.css";

function App() {
  useEspSync();
  useOrchestrationSync();
  useResourceSync();
  useSessionRestore();
  useAgentEvents();
  useNotifications();
  const onboarded = useStore((s) => s.onboarded);
  return (
    <div className="app">
      <div className="app-body">
        <LeftSidebar />
        <MainArea />
        <RightSidebar />
      </div>
      <StatusBar />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
