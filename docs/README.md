# CaPilot IDE

> AI Agent Orchestration Workbench — Tauri v2 + React + CodeMirror 6
> Agentic IDE：以「多个 AI agent 的创建、编排、指挥、监控」为中心，agent 终端（CLI TUI）与 Composer 智能输入层为主界面。

## Quick Start

```bash
pnpm install
pnpm tauri dev      # 开发（需 claude CLI；Linux 系统依赖见下）
pnpm tauri build    # 打包
cargo test          # 在 src-tauri/ 下运行 Rust 单元测试
pnpm tsc --noEmit   # TS 类型检查
```

Linux 系统依赖（一次性）：`libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`

## Docs

- **`CaPilot-IDE-RUNBOOK.md`** — 运行与维护手册（运行命令、设计资源、已知坑、安全注意）← 从这里开始
- `CaPilot-IDE-DevPlan.md` — 开发计划 v2.1（架构 / 模块 / 交互 / 里程碑 / 决策清单，已同步实施进度）
- `CaPilot-PRD.md` — 产品需求 v3.1（源：主仓库 `Doc/CaPilot-PRD.md`，改文档需两边同步）
- `security-review.md` — 安全审查
- `styleguide/` / `Assets/` — LUCY 设计规范与 logo 资产（源：主仓库 `Doc/`，改设计需两边同步）
- `CaPilot-IDE-Preview.html` — 界面参考（浏览器打开）
