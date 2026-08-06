# CaPilot IDE — 交接文档

> **日期:** 2026-08-05
> **状态:** 持续维护
> **对象:** 下一个接手 IDE 开发的开发者 / Claude 会话

本文是 CaPilot IDE（Tauri v2 桌面应用）的完整交接说明：项目方向、仓库结构、架构、功能进度、运行方式、已知问题与待办。接手者应先读本文，再读 `docs/CaPilot-IDE-DevPlan.md`（详细开发计划）与 `docs/security-review.md`（安全审查）。

---

## 1. 项目概述

CaPilot IDE 是 [CaPilot](https://github.com/hachi7574/CaPilot)（AI Agent 编排工作台，PRD v3.0）的桌面端产品主体。核心定位：

- **终端为中心的 agent 工作台**：每个 agent 是一个 PTY 终端（渲染其 CLI 的 TUI），底部 Composer 作为统一智能输入层
- **master/worker 编排**：master agent 通过 `capilot` PATH shim 派发任务给 worker，智能返回分级汇报
- **ESP 遥控器**：ESP32-C5 硬件作为遥控器（列表/设置/master 语音三模式），经 BLE NUS 与 IDE 通信
- **可替换的 agent 运行时**：claude / bash（已实现），codex / opencode / reasonix 等按 `AgentRuntimeAdapter` 一文件一 CLI 扩展

## 2. 技术栈

| 层 | 选型 | 备注 |
|----|------|------|
| 壳 | **Tauri v2** (Rust) | 系统 webview，非 Electron |
| 前端 | React 19 + TypeScript + Vite 7 | 包管理 pnpm |
| 状态 | Zustand 5 | |
| 编辑器 | CodeMirror 6 | `@codemirror/*` 全家桶 + `@codemirror/merge`（git diff）|
| 终端 | xterm.js 6 + addon-fit | |
| PTY | `portable-pty` 0.9 | wezterm 同款 |
| 持久化 | `rusqlite` (bundled) | sessions.db |
| 资源采样 | `sysinfo` | 进程树 CPU/内存 |
| BLE | `btleplug` | BlueZ/CoreBluetooth/Windows BT |
| 风格 | **LUCY styleguide**（8-bit Pixel × Apple Smooth）| 见下方 §7 |

## 3. 仓库结构

```
CaPilot-Ide/
├─ src-tauri/          # Rust 核心
│  ├─ src/
│  │  ├─ agent_runtime/   # adapter.rs(trait) / pty.rs / runtimes/(claude,bash)
│  │  ├─ orchestration/   # dispatcher(Unix socket) / shim(capilot) / smart_return
│  │  ├─ persistence.rs   # sqlite + .agent-meta.json + workspace layout
│  │  ├─ resource.rs      # sysinfo 采样器
│  │  ├─ esp/             # transport trait + ble(NUS) + protocol(帧) + manager
│  │  ├─ lib.rs           # 全部 Tauri 命令 + 插件注册
│  │  └─ bin/ble_test.rs  # 独立 BLE 联调工具
│  ├─ capabilities/default.json
│  ├─ tauri.conf.json
│  └─ Cargo.toml
├─ ui/                 # React 前端
│  ├─ components/
│  │  ├─ layout/          # LeftSidebar / RightSidebar / TabBar / ContentArea / Composer / StatusBar / SettingsModal / Onboarding
│  │  ├─ editor/          # EditorPanel (CM6 + autosave)
│  │  └─ terminal/        # XTermPanel
│  ├─ state/              # store.ts + agentActions / orchestration / esp / resource / notifications / session
│  ├─ App.tsx / main.tsx / App.css
├─ public/             # 字体 + logo
├─ docs/               # 本交接 + DevPlan + Preview + security-review
└─ package.json
```

## 4. 核心架构与数据流

### 4.1 Agent 生命周期

```
前端 Composer → invoke agent_spawn → Rust build_and_spawn
  → AgentRuntimeAdapter.spawn_interactive → (cmd, args)
  → PtyManager.spawn (portable-pty) → Channel<Vec<u8>> 流式输出
  → 前端 xterm (XTermPanel) 渲染
用户输入 → agent_write (文本+\r) → PTY stdin
```

- `AgentSession` 含 mode/speed/model/role/cwd/resume_key
- 会话持久化到 `~/CaPilot/workspaces/<项目>/`（context/agents + sessions.db）
- 重启恢复：`useSessionRestore` → `agent_resume`（幂等由 PtyManager 的 spawning token map 保证）

### 4.2 工作区 = 项目（聚焦模型）

- store 维护 `projects: string[]` + `projectRoots`（名→根路径）+ `focusedProject`
- 点击某项目终端 → 单选聚焦该项目 → TabBar 只显示该项目标签，文件树/Git 切换项目根
- 项目创建三种：新建文件夹 / 选现有文件夹 / git clone URL（`create_project` / `git_clone`）

### 4.3 编排（master/worker）

- `capilot` shim（`~/CaPilot/bin/`）→ Unix socket → Rust Dispatcher
- 命令：`capilot dispatch <worker> "任务"` / `status` / `report "摘要"`
- worker 池：busy 原子标记 + 3s 过期清扫；master 通过 socket 派发
- 智能返回：失败→完整 / ≤600→完整 / 600-3000→概述+关键文件 / >3000→标题级

### 4.4 ESP / BLE

- `EspTransport` trait（Ble/Usb/Wifi 三实现，BLE 默认）
- 帧协议：`CA50 | type | len | seq | version | payload | crc16`（IDE `protocol.rs` 与固件已对齐）
- `EspManager` 转发事件到前端 `esp://event`；状态栏/概览显示连接+电量
- 固件在 CaPilot 主仓库 `Firmware/PlatformIO/`（见 `docs/ESP-Firmware-Design.md`）

### 4.5 源代码管理（Git，按 VSCode SCM 重构）

- 面板（`RightSidebar.tsx` GitPanel）：头部（标题/当前分支/刷新/⋯ 菜单）+ 提交框 + 两组（暂存的更改 / 更改）
- **分组**：`git_status` 的 index vs worktree 字段拆分 —— staged = `index != ' ' && != '?'`，changes = `worktree != ' '`（MM 双状态会同时出现在两组，同 VSCode）
- **暂存/取消**：每文件 +/− 按钮；⋯ 菜单「全部暂存 / 全部取消暂存」；`git_stage`/`git_unstage`
- **放弃更改**：每文件 ⌫（confirm 后 `git_discard`：tracked→`git restore`，untracked→rm）；菜单「全部放弃更改」→ `git_discard_all`（`git restore .`，不动已暂存）
- **diff**：点文件行展开内联侧并 diff（`@codemirror/merge` MergeView）。内容来源：staged = `HEAD:` vs `:0:`，unstaged = `:0:` vs worktree，untracked = 空 vs worktree；「打开」→ 编辑器内全高 diff 标签页（`DiffPanel.tsx`，tab.type=`diff`），「编辑」→ 直接开源文件
- **提交**：提交框 Ctrl+Enter 提交；按钮 gating `msg.trim() && staged.length>0`；有 message 但未暂存时显示提示条
- **自动刷新**：面板挂载期间 2.5s 前端轮询 `refresh()`（VSCode 文件监听的对等实现，避免引入 Rust notify 原生依赖）；agents 落盘改动自动出现
- 后端：`git_discard`/`git_discard_all` 命令；`git_status` 未跟踪大文件改为流式数行数（`count_lines`）

## 5. 功能进度（对照 DevPlan）

| 阶段 | 内容 | 状态 |
|------|------|------|
| **P0** | 脚手架 / Rust 核心 / claude 适配 / CM6 / xterm / Composer | ✅ 完成（含大量交互细节）|
| **P0.5** | BLE NUS spike | ✅ 完成（超预期，直接到 P3 的 BLE 部分）|
| **P1** | contexts+sqlite / 角色 / runtime 切换 / capilot shim 编排 / 智能返回 / 文件树 / Git | ✅ 完成（Git 面板已按 VSCode SCM 重构）|
| **P2** | 资源监视 / 通知 / onboarding / 设置 / updater / 打包 / 安全审查 | ✅ 完成 |
| **P3** | EspTransport(BLE ✅ / USB/WiFi ❌) / 配对向导 ❌ / 心跳 ❌ / **语音链路 ❌** | 🟡 部分 |

## 6. 运行 / 构建

```bash
pnpm install
pnpm tauri dev      # 开发（需 claude CLI 已安装；系统依赖见 README）
pnpm tauri build    # 打包
cargo test          # Rust 测试（13 个）
pnpm tsc --noEmit   # TS 类型检查
```

Linux 系统依赖（一次性）：`libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`

## 7. 设计规范（LUCY styleguide）

IDE UI 遵循 CaPilot 主仓库 `Doc/styleguide/ui-style-guide.md` 的 LUCY 风格（深色科技 + 紫色强调）：

- **色彩**：`--bg #07090F`，`--brand #8B5CF6`（紫），状态色仅绿/黄/红
- **字体**：Silkscreen（像素标签）/ PixelifySans（标题）/ Tektur（正文）/ JetBrainsMono（技术）
- **边框**：2px 实线，硬阴影 `4px 4px 0`，几乎无圆角
- **动效**：Apple 曲线 `cubic-bezier(0.25,0.1,0.25,1)`
- 字体已内嵌在 `public/fonts/`；颜色令牌在 `ui/App.css :root`

> 注：styleguide 与 logo 资产已复制到本仓库 `docs/styleguide/`、`docs/Assets/`（与主仓库 `Doc/styleguide/`、`Doc/Assets/` 保持同步）。运行时字体/logo 在 `public/fonts/`、`public/*.png`，应用图标由主仓库 logo 生成于 `src-tauri/icons/`；改设计需两边同步。

## 8. 已知问题 / 待办

### 待开发（DevPlan P3 剩余）
- ESP：USB (`UsbSerial`) / WiFi (`WifiWs`) 传输、配对向导、5s 心跳/15s 超时、控制帧 ack/重试
- **语音链路**（最重）：ESP mic → Opus → BLE → Rust 解码 → sherpa-onnx 流式 STT → 实时字幕 → 回复 TTS
- 编辑器外部改动监视（notify → 前端刷新）：Git 面板已用 2.5s 轮询实现（见 §4.5），编辑器标签页本身仍未监听磁盘改动

### 已知技术债（Medium/Low）
- `.lock().unwrap()` 毒化处理（多处 std Mutex）
- dispatcher `reports` 日志无界增长（需环形缓冲）
- `git_status` 未跟踪大文件整读入内存（应流式）
- `resolve_worker` 前缀匹配歧义（短 id 可能派错）
- ESP `connected` 事件未带 `kind` 字段（前端 fallback 到 BLE）
- `Persistence::open` 启动 expect（HOME 不可写会 panic）
- 会话的 permissionMode 未持久化（重启回到 Ask）

### 安全注意事项
- `agent_write` / `esp_send` 是高权限命令，信任边界是"打包的前端受信任"
- `fs_*`/`git_*` 范围限制建议发布前收紧（见 `docs/security-review.md`）
- updater 配置是占位 endpoint/pubkey，发布前需填真实签名

## 9. 文档索引

| 文档 | 位置 | 内容 |
|------|------|------|
| 本交接 | `docs/HANDOVER.md` | 项目全貌 |
| 开发计划 | `docs/CaPilot-IDE-DevPlan.md` | DevPlan v2.0（架构/布局/里程碑）|
| 界面预览 | `docs/CaPilot-IDE-Preview.html` | 参考 UI 设计（浏览器打开）|
| 安全审查 | `docs/security-review.md` | CSP/权限/路径审查 |
| ESP 固件设计 | `docs/ESP-Firmware-Design.md`（源：主仓库 `Doc/ESP-Firmware-Design.md`）| 固件架构 + 协议 |
| ESP UI 对比 | `docs/ESP-UI-Framework-Comparison.md`（源：主仓库 `Doc/`）| WouoUI vs LVGL |
| 产品 PRD | `docs/CaPilot-PRD.md`（源：主仓库 `Doc/CaPilot-PRD.md`）| 产品整体需求 |
| LUCY 风格 | `docs/styleguide/`（源：主仓库 `Doc/styleguide/`）| 设计规范 |
