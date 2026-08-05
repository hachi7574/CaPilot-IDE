import { Titlebar } from "./components/layout/Titlebar";
import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import "./App.css";

function App() {
  return (
    <div className="app">
      <Titlebar />
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
