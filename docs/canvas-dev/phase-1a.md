# 期 1a — 空画布 tab + TabBar 右侧切换

> **状态:** 已落地（2026-08-20）
> **前置:** 无代码前置。读 [README.md](README.md) 的决策和锚点。
> **目标:** 点 TabBar 右侧按钮，主区在「当前项目的终端标签」和「该项目的空画布 tab」之间切换。没有节点、没有连线、没有运行。PTY 零变化。
> **提交:** `feat(canvas): Tab.type=canvas + TabBar view toggle + empty CanvasPanel`
> **不要做:** Rust IPC、卡片、xyflow、改 Composer 发送逻辑、session restore 打开画布 tab。

每步结束跑该步的「本步验收」。失败就停，不要跳。

---

## 步骤 1 — i18n 键

**改:** `ui/i18n/zh.ts`、`ui/i18n/en.ts`（en 必须同树，否则 `ZhMessages` 类型炸）。

**zh.ts `tabBar`（约 306 行）追加：**

```ts
canvasView: "画布视角",
terminalView: "终端视角",
```

**zh.ts `leftSidebar`（约 370 行，`newTerminal` 旁）追加：**

```ts
openCanvas: "打开画布",
```

**zh.ts 在 `content` 块后（或文件合适位置）新增整块：**

```ts
canvas: {
  tabTitle: "{project} 画布",
  emptyHint: "当前工作区还没有终端积木。新建终端后会作为卡片出现在这里。",
  openTerminal: "打开终端",
  noProject: "先创建一个项目再打开画布",
},
```

**en.ts 对应：**

```ts
tabBar.canvasView: "Canvas view"
tabBar.terminalView: "Terminal view"
leftSidebar.openCanvas: "Open canvas"
canvas: {
  tabTitle: "{project} canvas",
  emptyHint: "No terminal blocks in this workspace yet. New terminals show up here as cards.",
  openTerminal: "Open terminal",
  noProject: "Create a project before opening the canvas",
}
```

**本步验收:**

```bash
pnpm tsc --noEmit
```

只加键、不引用，也应过。en 漏键会在 `ZhMessages` 上报错。

---

## 步骤 2 — 扩 `Tab` 类型

**改:** `ui/state/store.ts:327-338`

把

```ts
type: "agent" | "editor" | "diff" | "image";
```

改成

```ts
type: "agent" | "editor" | "diff" | "image" | "canvas";
```

在 `title: string;` 前或后加：

```ts
/** canvas / 分组：所属项目名。agent 仍以 AgentInfo.project 为准。 */
project?: string;
```

`filePath` 注释补一句：canvas 复用它存 `workspaceId`（绝对路径或项目根），不是文件。

**不要**改 `addTab` / `closeTab` / `setActiveTab`。现有按 `id` 去重已经够用。

**本步验收:** `pnpm tsc --noEmit`。全仓库没有 exhaustive switch 因多一个 kind 而炸；若有，补 `"canvas"` 分支（常见在 TabBar 图标三元、ContentArea Panel）。这一步先允许 TS 因「未处理 canvas」报错——下一步会补。若这一步 tsc 已红，记下报错文件，步骤 4–6 必须清掉。

---

## 步骤 3 — `ui/state/canvas.ts`（新文件）

新建。不要放进 store 本体。只依赖 `useStore` 和 `t()`。

```ts
import { useStore, type Tab } from "./store";
import { t } from "../i18n";

export interface CanvasScope {
  projectId: string;
  workspaceId: string;
}

export function canvasTabId(scope: CanvasScope): string {
  return `canvas:${scope.workspaceId}`;
}

export function canvasTab(scope: CanvasScope): Tab {
  return {
    id: canvasTabId(scope),
    type: "canvas",
    title: t("canvas.tabTitle", { project: scope.projectId }),
    project: scope.projectId,
    filePath: scope.workspaceId,
  };
}

function projectOfCwd(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** 当前画布作用域：focusedProject → 当前 tab 的项目 → "default"。 */
export function resolveCanvasScope(): CanvasScope {
  const s = useStore.getState();
  let projectId = s.focusedProject;
  if (!projectId) {
    const tab = s.tabs.find((t) => t.id === s.activeTabId);
    if (tab?.type === "canvas" && tab.project) projectId = tab.project;
    else if (tab?.type === "agent" && tab.agentId) {
      const agent = s.agents.get(tab.agentId);
      projectId = agent?.project ?? (agent?.cwd ? projectOfCwd(agent.cwd) : undefined);
    } else if (tab?.project) {
      projectId = tab.project;
    } else if (tab?.filePath) {
      // editor/diff/image: 最长 projectRoots 前缀
      let best: string | undefined;
      let bestLen = -1;
      for (const [name, root] of Object.entries(s.projectRoots)) {
        const prefix = root.endsWith("/") ? root : `${root}/`;
        if (tab.filePath.startsWith(prefix) && prefix.length > bestLen) {
          best = name;
          bestLen = prefix.length;
        }
      }
      projectId = best;
    }
  }
  projectId = projectId ?? "default";
  const root = s.projectRoots[projectId];
  const wt = root ? s.worktrees.find((w) => w.path === root) : undefined;
  const workspaceId = wt?.path ?? root ?? projectId;
  return { projectId, workspaceId };
}

const returnTabByScope = new Map<string, string>();

export function rememberCanvasReturnTab(scope: CanvasScope, tabId: string | null): void {
  if (!tabId) return;
  const s = useStore.getState();
  const tab = s.tabs.find((t) => t.id === tabId);
  if (!tab || tab.type === "canvas") return;
  returnTabByScope.set(canvasTabId(scope), tabId);
}

export function takeCanvasReturnTab(scope: CanvasScope): string | null {
  const key = canvasTabId(scope);
  const id = returnTabByScope.get(key) ?? null;
  if (!id) return null;
  const s = useStore.getState();
  if (!s.tabs.some((t) => t.id === id)) {
    returnTabByScope.delete(key);
    return null;
  }
  return id;
}

export function openCanvas(scope?: CanvasScope): string {
  const s = useStore.getState();
  const resolved = scope ?? resolveCanvasScope();
  const id = canvasTabId(resolved);
  if (s.tabs.some((t) => t.id === id)) {
    s.setActiveTab(id);
    return id;
  }
  s.addTab(canvasTab(resolved));
  return id;
}

export function toggleCanvasView(): void {
  const s = useStore.getState();
  const scope = resolveCanvasScope();
  const id = canvasTabId(scope);
  if (s.activeTabId === id) {
    const prev = takeCanvasReturnTab(scope);
    if (prev) s.setActiveTab(prev);
    else s.closeTab(id);
    return;
  }
  rememberCanvasReturnTab(scope, s.activeTabId);
  openCanvas(scope);
}
```

规则：

- **不要**在 `ui/state/session.ts` 里 `addTabSilent` 画布 tab。
- 返回 tab 只记内存 Map，不持久化。
- `closeTab(id)` 关的是 tab，不是 `closeAgentAction`。canvas 没有 `agentId`，不会杀 PTY。

**本步验收:** `pnpm tsc --noEmit`。此文件本身应通过。TabBar 还没引用它。

---

## 步骤 4 — 空 `CanvasPanel`

**新建** `ui/components/canvas/CanvasPanel.tsx`：

```tsx
import { useT } from "../../i18n";
import type { CanvasScope } from "../../state/canvas";

export function CanvasPanel({
  scope,
  active: _active,
}: {
  scope: CanvasScope;
  active?: boolean;
}) {
  const t = useT();
  return (
    <div className="canvas-panel">
      <div className="canvas-empty">{t("canvas.emptyHint")}</div>
    </div>
  );
}
```

**`ui/App.css` 追加（Content Area 段落后即可）：**

```css
.canvas-panel {
  flex: 1;
  display: flex;
  overflow: hidden;
  min-width: 0;
  background: var(--term-bg);
}
.canvas-empty {
  margin: auto;
  color: var(--ink2);
  font-family: var(--mono);
  font-size: var(--fs-sm);
  max-width: 36em;
  text-align: center;
  padding: 24px;
}
```

**不要** import `XTermPanel`。不要 `data-tauri-drag-region`。

**本步验收:** 文件存在；`tsc` 仍可能因 ContentArea 未引用而过。

---

## 步骤 5 — `ContentArea` 分支 + Ctrl+F 跳过

**改:** `ui/components/layout/ContentArea.tsx`

1. 顶部 import：

```ts
import { CanvasPanel } from "../canvas/CanvasPanel";
```

2. `Panel` 函数里，`tab.type === "diff"` 分支后面追加：

```tsx
{tab.type === "canvas" && (
  <CanvasPanel
    scope={{
      projectId: tab.project ?? "default",
      workspaceId: tab.filePath ?? tab.project ?? "default",
    }}
    active={active}
  />
)}
```

3. Ctrl+F 处理（约 246–254 行）保持：只对 `editor` / `agent` 调 `requestSearch`。canvas 走完 `if (!activeTab) return;` 之后两个 if 都不进，等于 no-op。**不要**给 canvas 加搜索。显式写注释：

```ts
// canvas / image / diff: no in-panel search in v1
```

**不要**改 Ctrl+T。

**本步验收:** `pnpm tsc --noEmit`。ContentArea 不再因未处理 `"canvas"` 报错。

---

## 步骤 6 — TabBar：分组、图标、右侧按钮

**改:** `ui/components/layout/TabBar.tsx`、`ui/App.css`；可能新建 `ui/assets/icons/layout-grid.svg`。

### 6.1 图标文件

仓库已有 `square-terminal.svg`。没有 `layout-grid.svg` 时，从 Lucide `layout-grid` 拷到 `ui/assets/icons/layout-grid.svg`，格式对齐现有文件（`stroke="currentColor"` + `style="stroke:var(--icon-color,currentColor)"`）。`Icon.tsx` 的 glob 会自动收录。**禁止 emoji。** 暂时也可用已有 `network.svg`，但文档和 i18n 按 `layout-grid` 写。

### 6.2 `tabProject`（约 78–98 行）

在 `tab.type === "agent"` 之后、`editor|diff|image` 之前插入：

```ts
if (tab.type === "canvas") {
  return tab.project ?? (tab.filePath ? projectOf(tab.filePath) : undefined);
}
```

### 6.3 图标三元（约 607–614 行）

```tsx
<Icon
  name={
    tab.type === "agent"
      ? runtimeIcon(agent?.runtime ?? "")
      : tab.type === "image"
        ? "image"
        : tab.type === "canvas"
          ? "layout-grid"
          : "file-text"
  }
  size={12}
  style={{ marginRight: 5 }}
/>
```

`diff` 仍走 `file-text`。

### 6.4 `closeAllFiles`（约 226–231 行）

**不要**加 `"canvas"`。画布不是文件。关 canvas 走现有 `else closeTab(tab.id)`（约 213 行），不会进 `closeAgentAction`。

### 6.5 右侧切换按钮

在组件里取：

```ts
import { toggleCanvasView } from "../../state/canvas";

const projects = useStore((s) => s.projects);
const activeTab = tabs.find((t) => t.id === activeTabId);
```

在 `.tab-add` 按钮之后、`{!leftSidebarOpen && ( <div className="tab-win-controls">` 之前插入：

```tsx
<button
  type="button"
  className={`tab-view-toggle${activeTab?.type === "canvas" ? " active" : ""}`}
  title={
    projects.length === 0
      ? t("canvas.noProject")
      : activeTab?.type === "canvas"
        ? t("tabBar.terminalView")
        : t("tabBar.canvasView")
  }
  aria-pressed={activeTab?.type === "canvas"}
  disabled={projects.length === 0}
  onClick={(e) => {
    e.stopPropagation();
    toggleCanvasView();
  }}
>
  <Icon
    name={activeTab?.type === "canvas" ? "square-terminal" : "layout-grid"}
    size={14}
  />
</button>
```

按钮必须能点：父级 `.tab-bar` 有 `data-tauri-drag-region`，所以 CSS 要 `-webkit-app-region: no-drag`。

### 6.6 CSS（`App.css`，紧跟 `.tab-add`）

**窗口控件抢 `margin-left: auto`。不要两个都 auto。**

现有 `.tab-win-controls { margin-left: auto; }` 改成 `margin-left: 0;`（toggle 负责把整组推到右边）。

```css
.tab-view-toggle {
  width: 28px; height: 26px;
  display: flex; align-items: center; justify-content: center;
  color: var(--ink2); cursor: pointer; flex-shrink: 0;
  background: none; border: 1px solid transparent;
  margin-left: auto;
  -webkit-app-region: no-drag;
  transition: color .2s var(--ease-apple), border-color .2s var(--ease-apple);
}
.tab-view-toggle:hover { color: var(--brand); border-color: var(--rule2); }
.tab-view-toggle.active { color: var(--brand); border-color: var(--brand); }
.tab-view-toggle:disabled { opacity: .4; cursor: default; }

.tab-win-controls {
  /* 覆盖原 margin-left: auto —— toggle 已经 auto 把这组钉右 */
  margin-left: 0;
}
```

左侧栏开着时没有 `.tab-win-controls`，toggle 自己的 `margin-left: auto` 仍钉右。收起时 toggle auto + 窗口控件跟在后面。

**本步验收:** `pnpm tsc --noEmit`。肉眼：`+` 右侧出现按钮；左侧栏收起时按钮在 min/max/close **左侧**，不挤掉窗口按钮。

---

## 步骤 7 — 次入口：项目菜单

**改:** `ui/components/layout/LeftSidebar.tsx`

1. import `openCanvas`。
2. 项目 `ContextMenu` 里，「新建终端」那一项（约 1387–1400 行）**下面**插入：

```tsx
<div
  className="ctx-item"
  onClick={() => {
    openCanvas({ projectId: proj, workspaceId: projRoot ?? proj });
    onClose();
  }}
>
  <Icon name="layout-grid" size={13} /> {t("leftSidebar.openCanvas")}
</div>
```

`projRoot` 已在该分支算过（`projectRoots[proj] ?? ctx.cwd`）。

主入口永远是 TabBar 按钮。菜单只是备份。

**本步验收:** `tsc` 过。先不手动点也行，步骤 9 一起点。

---

## 步骤 8 — 类型与静态检查

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
```

Rust 不应有失败（本期零 Rust 改动）。若 `tsc` 红：

- `Tab.type` exhaustive → 补 canvas 分支
- `ZhMessages` → en 漏键
- `Icon name="layout-grid"` 若类型是 union：确认 svg 文件名 stem 为 `layout-grid`（`Icon.tsx` 实际是 `string`，不会炸）

---

## 步骤 9 — 手动验收（必须全过）

`pnpm tauri dev`，用任意已有项目（或测试仓库）：

1. 有项目时，TabBar 右侧出现切换按钮（`+` 和窗口按钮之间 / 最右侧）。
2. 点一次：打开 **一张** `canvas:<workspaceId>` tab，主区是空画布文案。
3. 再点一次：回到进入前的 agent/editor tab。画布 tab **建议留下**（再点只 `setActiveTab`，不新建）。
4. 同一作用域连点不会出现第二张画布 tab。
5. 切换 `focusedProject` 后再点按钮：打开的是**新**作用域画布（另一张 tab）。旧画布按 `tabProject` 过滤隐藏，不是关掉。
6. 右键任意 tab →「关闭所有文件」：editor/image/diff 关了，画布 tab 还在，终端还在。
7. 关掉画布 tab：侧栏终端 / PTY 都还活着（状态不是「已结束」）。
8. 画布激活时 Composer 没有 agent 目标（显示无标签或走现有 spawn 逻辑）。**不要**改发送去认 canvas。
9. 无项目时按钮 disabled，title 是「先创建一个项目…」。
10. 中/英切换：按钮 title 跟着变。无 emoji。
11. 收起左侧栏：切换按钮在窗口 min/max/close 左侧，窗口按钮仍可点。
12. 项目右键有「打开画布」，效果同 TabBar 按钮。

---

## 步骤 10 — 收尾文档

- `docs/canvas-view.md` 状态行：`设计稿` → `期 1a 已落地`
- `docs/CaPilot-IDE-RUNBOOK.md` 加一句：画布入口 = TabBar 右侧终端/画布切换
- 本文件顶部加一行：`状态: 已落地`

---

## 完成清单（期 1a 出门）

- [ ] `Tab.type` 含 `"canvas"`
- [ ] `pnpm tsc --noEmit` 绿
- [ ] `cargo test` 绿（无回归）
- [ ] 步骤 9 的 12 条手动全过
- [ ] 无 Rust 新 command
- [ ] `ui/components/canvas/` 里没有 `XTermPanel`
- [ ] session restore 仍只恢复 agent tab

过了才能开 [phase-1b.md](phase-1b.md)。
