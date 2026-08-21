# CaPilot IDE Documentation

CaPilot IDE is a local AI coding workspace centered on interactive terminal sessions, file editing, and Git.

## Quick Start

```bash
pnpm install
pnpm tauri dev
pnpm tauri build
cd src-tauri && cargo test
```

Linux system dependencies: `libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`.

## Docs

- `CaPilot-IDE-RUNBOOK.md` — running, maintenance, known issues（含 2026-08-18 权限默认/拖拽/F1/todo 完成等已落地记录；Linux 打包视频壁纸走 loopback HTTP）, and security notes
- `security-review.md` — security review and release checklist
- `ai-runtime-references.md` — official docs + hard-coded integration facts for the claude / codex / opencode / dsh / pi runtimes（权限 flag、hook 注入）
- `styleguide/` — LUCY design guide；图标/logo/app-icon 统一放在 `ui/assets/`（`icons/`、`logo/`、`app-icon/`）

## SVG 图标集（GUI 图标标准）

**规则：GUI 一律不允许使用 emoji，全部使用 SVG 图标。**

- 图标源文件：`ui/assets/icons/` — 根目录 104 个 Lucide 线性图标（`stroke` + `currentColor`），`brands/` 子目录 7 个品牌单色 logo（`fill="currentColor"`，来源 Simple Icons）。
- React 组件：`ui/components/Icon.tsx`（**构建时由 Vite `import.meta.glob` 直接加载 `ui/assets/icons/*.svg`，新增/删除 SVG 即生效，无需生成步骤**）。用法：`import { Icon } from "../Icon"`，`<Icon name="activity" size={16} />`。运行时 logo 用 `runtimeIcon(runtime)`：`bash→gnubash`、`claude→claude`、`codex→openai`、`opencode→opencode`、其它→`terminal`。
- 颜色：默认继承元素 `color`（CSS 变量 `--icon-color: currentColor`）；在任意作用域设置 `--icon-color: <color>` 即可整体换色。
- 完整 emoji → SVG 文件 → lucide 图标名对照表：见 `ui/assets/icons/README.md`。
