import { AnnotationLayer } from "./AnnotationLayer";
import { AnnotationTray } from "./AnnotationTray";
import "./annotations.css";

/** Dev-only design-feedback chrome. Loaded only via dynamic import from App. */
export function DevAnnotations({ onHide }: { onHide?: () => void }) {
  return (
    <>
      <AnnotationLayer />
      <AnnotationTray onHide={onHide} />
    </>
  );
}
