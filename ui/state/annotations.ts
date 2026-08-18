import { create } from "zustand";
import { t } from "../i18n";

// ── Types ───────────────────────────────────────────────────────

export type AnnotIntent = "change" | "question" | "error";

export interface AnnotElementInfo {
  tag: string;
  id?: string;
  classes: string[];
  /** Trimmed, whitespace-collapsed text snippet (≤ 140 chars). */
  text: string;
  /** CSS selector path from the root down to the element. */
  selector: string;
  /** Nearest named React component (empty when none found). */
  component: string;
}

export interface Annotation {
  id: string;
  /** Live resolver: re-attaches the numbered marker when the UI re-renders. */
  selector: string;
  /** Snapshot captured at click time — the copied markdown keeps it even if the
   *  element (or its selector) disappears from the live DOM. */
  element: AnnotElementInfo;
  text: string;
  intent: AnnotIntent;
  createdAt: number;
}

// ── Persistence ────────────────────────────────────────────────

const STORAGE_KEY = "capilot.annotations";

function loadAnnotations(): Annotation[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Annotation[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function saveAnnotations(list: Annotation[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(list));
  } catch {
    // storage unavailable — annotations stay in memory only
  }
}

// ── Store ──────────────────────────────────────────────────────

interface AnnotateState {
  /** Crosshair selection mode. */
  mode: boolean;
  annotations: Annotation[];
  /** Most recently picked element (used when there are no comments: copying
   *  yields the element's own component info instead of a feedback block). */
  lastElement: AnnotElementInfo | null;
  toggleMode: () => void;
  setMode: (m: boolean) => void;
  add: (a: Annotation) => void;
  remove: (id: string) => void;
  clear: () => void;
  setLastElement: (info: AnnotElementInfo | null) => void;
}

export const useAnnotations = create<AnnotateState>((set) => ({
  mode: false,
  annotations: loadAnnotations(),
  lastElement: null,

  toggleMode: () => set((s) => ({ mode: !s.mode })),
  setMode: (m) => set({ mode: m }),

  add: (a) =>
    set((s) => {
      const annotations = [...s.annotations, a];
      saveAnnotations(annotations);
      return { annotations, lastElement: a.element };
    }),

  remove: (id) =>
    set((s) => {
      const annotations = s.annotations.filter((x) => x.id !== id);
      saveAnnotations(annotations);
      return { annotations };
    }),

  clear: () => {
    saveAnnotations([]);
    set({ annotations: [], mode: false });
  },

  setLastElement: (info) => set({ lastElement: info }),
}));

// ── Element introspection ──────────────────────────────────────
// Best-effort identity capture for a clicked element: a CSS selector path plus
// the nearest named React component (walked up from the host node's fiber).

const SEMANTIC_TAGS = new Set([
  "button", "a", "input", "select", "textarea", "img", "h1", "h2", "h3",
  "h4", "h5", "h6", "li", "table", "tr", "td", "form", "label", "video",
  "audio", "code", "pre",
]);

/** Container shells that are useless to annotate on their own — keep climbing. */
const SKIP_CLASSES = new Set([
  "content-panel", "pane-shell", "split-container", "split-pane", "app", "app-body",
]);

/** Climb to the nearest element worth annotating at (x, y). Returns null when
 *  the point is over the annotator's own UI. */
export function pickElement(x: number, y: number): HTMLElement | null {
  const raw = document.elementFromPoint(x, y);
  if (!raw) return null;
  let el = raw instanceof HTMLElement ? raw : raw.parentElement;
  while (el && el !== document.body) {
    if (el.closest("[data-annot-ui]")) return null;
    if (isMeaningful(el)) return el;
    el = el.parentElement;
  }
  return el && el !== document.body && isMeaningful(el) ? el : null;
}

function isMeaningful(el: HTMLElement): boolean {
  const tag = el.tagName.toLowerCase();
  if (tag === "html" || tag === "body" || tag === "canvas" || tag === "svg" || tag === "path") {
    return false;
  }
  if (el.hasAttribute("data-annot-ui")) return false;
  const rect = el.getBoundingClientRect();
  if (rect.width < 4 || rect.height < 4) return false;
  if (SEMANTIC_TAGS.has(tag)) return true;
  if (el.id) return true;
  for (const c of el.classList) {
    if (!SKIP_CLASSES.has(c)) return true;
  }
  return false;
}

/** Build a (best-effort) unique CSS path for an element: id wins outright, else
 *  tag + first class + :nth-of-type repeated up to the root. */
export function cssPath(el: Element): string {
  if (!(el instanceof Element)) return "";
  const parts: string[] = [];
  let node: Element | null = el;
  let guard = 0;
  while (node && node.nodeType === 1 && guard < 20) {
    guard += 1;
    const current: Element = node;
    const tag = current.tagName.toLowerCase();
    if (tag === "html") break;
    if (current.id) {
      parts.unshift(`#${CSS.escape(current.id)}`);
      break;
    }
    let sel = tag;
    if (current.classList.length) sel += `.${CSS.escape(current.classList[0])}`;
    const parentEl: Element | null = current.parentElement;
    if (parentEl) {
      let index = 1;
      let matches = 0;
      const children = Array.from(parentEl.children);
      for (const child of children) {
        if (child.tagName === current.tagName) matches += 1;
      }
      if (matches > 1) {
        for (const child of children) {
          if (child === current) break;
          if (child.tagName === current.tagName) index += 1;
        }
        sel += `:nth-of-type(${index})`;
      }
    }
    parts.unshift(sel);
    node = parentEl;
  }
  return parts.length ? parts.join(" > ") : (el.tagName || "").toLowerCase();
}

const FIBER_PREFIX = "__reactFiber$";

function fiberFor(el: Element): unknown {
  const anyEl = el as unknown as Record<string, unknown>;
  for (const key of Object.keys(anyEl)) {
    if (key.startsWith(FIBER_PREFIX)) return anyEl[key];
  }
  return null;
}

function componentNameForType(type: unknown): string {
  if (typeof type === "string") return "";
  if (typeof type === "function") {
    const fn = type as { displayName?: string; name?: string };
    return fn.displayName || fn.name || "";
  }
  if (type && typeof type === "object") {
    const t = type as { displayName?: string; name?: string; type?: unknown };
    if (t.displayName) return t.displayName;
    if (typeof t.name === "string" && t.name) return t.name;
    if (typeof t.type === "function") return componentNameForType(t.type);
  }
  return "";
}

/** Nearest named React component owning a DOM node (via the host fiber). */
export function findComponentName(el: Element): string {
  let fiber = fiberFor(el) as {
    return?: unknown;
    type?: unknown;
  } | null;
  const seen = new Set<unknown>();
  let guard = 0;
  while (fiber && guard < 40) {
    guard += 1;
    if (seen.has(fiber)) break;
    seen.add(fiber);
    const name = componentNameForType(fiber.type);
    const internal = name === "Memo" || name === "ForwardRef" || name === "Context";
    if (name && !internal) return name;
    fiber = fiber.return as typeof fiber;
  }
  return "";
}

/** Snapshot an element's identity at click time. */
export function elementInfo(el: Element): AnnotElementInfo {
  const text = (el.textContent || "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 140);
  return {
    tag: el.tagName.toLowerCase(),
    id: el.id || undefined,
    classes: Array.from(el.classList),
    text,
    selector: cssPath(el),
    component: findComponentName(el),
  };
}

/** Resolve a stored selector back to a live element (best-effort). */
export function resolveBySelector(selector: string): Element | null {
  try {
    return document.querySelector(selector);
  } catch {
    return null;
  }
}

// ── Markdown builders ──────────────────────────────────────────

function intentLabel(intent: AnnotIntent): string {
  return t(`annotations.${intent}`);
}

/** `## Design Feedback` block for all collected comments. */
export function buildFeedbackMarkdown(list: Annotation[]): string {
  const head = ["## Design Feedback", "", `> ${t("annotations.pageLabel")}`];
  const body = list.map((a, i) => {
    const e = a.element;
    const [firstLine, ...restLines] = a.text.split("\n");
    const title = firstLine.trim() || t("annotations.noDescription");
    const rest = restLines.join("\n").trim();
    const label = intentLabel(a.intent);
    const unknown = t("annotations.unknownComponent");
    const lines = [
      `### ${i + 1}. [${label}] ${title}`,
      `- **${t("annotations.intent")}**: ${label}`,
      `- **${t("annotations.component")}**: \`${e.component || unknown}\``,
      `- **${t("annotations.element")}**: \`<${e.tag}${e.classes.length ? ` class="${e.classes.slice(0, 4).join(" ")}"` : ""}>\``,
      `- **${t("annotations.selector")}**: \`${e.selector}\``,
    ];
    if (e.id) lines.push(`- **id**: \`${e.id}\``);
    if (e.text) lines.push(`- **${t("annotations.textLabel")}**: "${e.text}"`);
    if (rest) lines.push("", rest);
    return lines.join("\n");
  });
  return [...head, "", ...body].join("\n");
}

/** Single element's component info (used when there are no comments). */
export function buildElementMarkdown(info: AnnotElementInfo): string {
  const unknown = t("annotations.unknownComponent");
  const lines = [
    `## ${t("annotations.elementInfoTitle")}`,
    `- **${t("annotations.component")}**: \`${info.component || unknown}\``,
    `- **${t("annotations.tag")}**: \`<${info.tag}>\``,
  ];
  if (info.classes.length) {
    lines.push(`- **${t("annotations.classLabel")}**: \`${info.classes.join(" ")}\``);
  }
  if (info.id) lines.push(`- **id**: \`${info.id}\``);
  if (info.text) lines.push(`- **${t("annotations.textLabel")}**: "${info.text}"`);
  lines.push(`- **${t("annotations.selector")}**: \`${info.selector}\``);
  return lines.join("\n");
}

// ── Clipboard / screenshot ──────────────────────────────────────

/** Copy text to the OS clipboard (navigator API with textarea fallback for
 *  WebKit webviews that reject the async API). */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // fall through to the legacy path
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  document.execCommand("copy");
  document.body.removeChild(ta);
}

function collectCss(): string {
  let css = "";
  for (const sheet of Array.from(document.styleSheets)) {
    try {
      const rules = sheet.cssRules;
      if (!rules) continue;
      for (const rule of Array.from(rules)) css += `${rule.cssText}\n`;
    } catch {
      // cross-origin stylesheet — skip
    }
  }
  return css;
}

/** Rasterize a live element to a PNG blob via SVG foreignObject. Injected CSS
 *  is copied inline so the clone renders (approximately) like the real node.
 *  Canvas nodes (xterm) and annotator chrome are stripped from the clone. */
export async function captureElementToPng(el: Element): Promise<Blob> {
  const rect = el.getBoundingClientRect();
  const w = Math.max(1, Math.round(rect.width));
  const h = Math.max(1, Math.round(rect.height));

  const clone = el.cloneNode(true) as HTMLElement;
  clone.querySelectorAll("canvas").forEach((c) => c.remove());
  clone.querySelectorAll("[data-annot-ui], [data-annot-overlay]").forEach((c) => c.remove());

  const style = document.createElement("style");
  style.textContent = collectCss();
  const head = document.createElement("head");
  head.appendChild(style);
  const body = document.createElement("body");
  body.style.cssText = "margin:0;padding:0;overflow:hidden;background:transparent";
  const wrapper = document.createElement("div");
  wrapper.style.cssText = `position:relative;width:${w}px;height:${h}px`;
  wrapper.appendChild(clone);
  body.appendChild(wrapper);
  const html = document.createElement("html");
  html.appendChild(head);
  html.appendChild(body);

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  svg.setAttribute("width", String(w));
  svg.setAttribute("height", String(h));
  const fo = document.createElementNS("http://www.w3.org/2000/svg", "foreignObject");
  fo.setAttribute("width", "100%");
  fo.setAttribute("height", "100%");
  fo.appendChild(html);
  svg.appendChild(fo);

  const data = new XMLSerializer().serializeToString(svg);
  const img = new Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(data)}`;
  await new Promise<void>((resolve, reject) => {
    img.onload = () => resolve();
    img.onerror = () => reject(new Error(t("annotations.renderFailed")));
  });
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error(t("annotations.canvasUnavailable"));
  ctx.drawImage(img, 0, 0, w, h);
  const blob = await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error(t("annotations.pngFailed")))),
      "image/png"
    );
  });
  return blob;
}

/** Copy an element screenshot to the OS clipboard as a PNG image. Falls back to
 *  copying the PNG as a base64 data-URL text when the ClipboardItem API rejects
 *  it (older WebKit). */
export async function copyElementScreenshot(el: Element): Promise<"image" | "dataurl" | "error"> {
  try {
    const blob = await captureElementToPng(el);
    if (typeof ClipboardItem !== "undefined" && navigator.clipboard.write) {
      try {
        await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
        return "image";
      } catch {
        // fall through to data-URL text
      }
    }
    const dataUrl = await new Promise<string>((resolve, reject) => {
      const fr = new FileReader();
      fr.onload = () => resolve(String(fr.result));
      fr.onerror = () => reject(new Error(t("annotations.readShotFailed")));
      fr.readAsDataURL(blob);
    });
    await copyText(dataUrl);
    return "dataurl";
  } catch {
    return "error";
  }
}
