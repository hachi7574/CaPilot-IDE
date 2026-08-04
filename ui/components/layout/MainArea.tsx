import { TabBar } from "./TabBar";
import { ContentArea } from "./ContentArea";

export function MainArea() {
  return (
    <div className="main-area">
      <TabBar />
      <ContentArea />
    </div>
  );
}
