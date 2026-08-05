import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { useEspSync } from "./state/esp";
import { useOrchestrationSync } from "./state/orchestration";
import { useSessionRestore } from "./state/session";
import "./App.css";

function App() {
  useEspSync();
  useOrchestrationSync();
  useSessionRestore();
  return (
    <div className="app">
      <div className="app-body">
        <LeftSidebar />
        <MainArea />
        <RightSidebar />
      </div>
      <StatusBar />
    </div>
  );
}

export default App;
