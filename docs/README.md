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

- `CaPilot-IDE-RUNBOOK.md` — running, maintenance, known issues, and security notes
- `security-review.md` — security review and release checklist
- `ai-runtime-references.md` — official docs + hard-coded integration facts for the claude / codex / opencode runtimes
- `dsh-runtime-integration.md` — dsh-TUI（DeepSeek Harness）runtime 集成设计：适配面、trait 逐项映射、状态钩子缺口、实施计划，附自主 agent 执行用的验收标准 / 自主权限 / 验证循环 / 交接手记；**顶部含「§0 前置条件：安装 dsh 运行时」**（CLI / profile / 凭据 / settings.yaml 的安装步骤）
- `structured-agent-runtime-architecture.md` — ACP-first structured Agent provider architecture, unified sessions/events, security boundaries, and migration plan
- `keyboard-shortcuts.md` — 快捷键速查：bash 终端 + claude / codex / opencode 全部 runtime
- `styleguide/` / `Assets/` — LUCY design guide and assets

## SVG 图标集（GUI 图标标准）

**规则：GUI 一律不允许使用 emoji，全部使用 SVG 图标。**

- 图标源文件：`docs/Assets/Icons/` — 根目录 104 个 Lucide 线性图标（`stroke` + `currentColor`），`brands/` 子目录 7 个品牌单色 logo（`fill="currentColor"`，来源 Simple Icons）。
- React 组件：`ui/components/Icon.tsx`（**由脚本生成，勿手改**）。用法：`import { Icon } from "../Icon"`，`<Icon name="activity" size={16} />`。运行时 logo 用 `runtimeIcon(runtime)`：`bash→gnubash`、`claude→claude`、`codex→openai`、`opencode→opencode`、其它→`terminal`。
- 颜色：默认继承元素 `color`（CSS 变量 `--icon-color: currentColor`）；在任意作用域设置 `--icon-color: <color>` 即可整体换色。
- 完整 emoji → SVG 文件 → lucide 图标名对照表：见 `docs/Assets/Icons/README.md`。
