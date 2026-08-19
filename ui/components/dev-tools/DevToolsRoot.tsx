import { DevToolsHost } from "./DevToolsHost";
import { DevAnnotations } from "../annotations/DevAnnotations";
import { useAnnotations } from "../../state/annotations";

/**
 * Dev-only floating annotations tray. Dynamically imported from App.tsx
 * behind `import.meta.env.DEV` so production builds eliminate this graph.
 * Theme Editor is a production feature and mounts separately via ThemeLabGate.
 */
export function DevToolsRoot() {
  const annotCount = useAnnotations((s) => s.annotations.length);

  return (
    <DevToolsHost
      tools={[
        {
          id: "annotations",
          labelKey: "annotations.tray",
          showKey: "annotations.show",
          badge: annotCount,
          render: ({ onHide }) => <DevAnnotations onHide={onHide} />,
        },
      ]}
    />
  );
}
