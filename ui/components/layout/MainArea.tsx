import { TabBar } from "./TabBar";
import { ContentArea } from "./ContentArea";
import { Composer } from "./Composer";

export function MainArea() {
  return (
    <div className="main-area">
      <TabBar />
      <ContentArea />
      <Composer />
    </div>
  );
}
