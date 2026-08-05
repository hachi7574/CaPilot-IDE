# CaPilot IDE 开发计划

> **版本:** v2.0（自建 Tauri 路线）
> **日期:** 2026-08-04
> **作者:** HaChi + Claude
> **关联:** [CaPilot-PRD.md](CaPilot-PRD.md) v3.0
> **状态:** Draft

---

## 1. 背景与决策

CaPilot IDE 是 CaPilot v3.0 的产品主体（AI Agent 编排工作台）。核心产品约束：

- worktree-per-agent **不重要**，不需要 branch 隔离（改为 contexts 目录模型）
- agent 运行时**必须可替换**：不局限于 Claude Code CLI，claude/codex/opencode/reasonix/zcode 皆可
- 不捆绑 Chromium（轻内存），但必须是**真桌面应用**（双击启动、独立窗口、托盘，不依赖打开浏览器）
- Monaco 非硬性依赖，选更适配 webview 框架的编辑器（CodeMirror 6）

### 1.1 决策表

| 决策项 | 结论 | 原因 |
| --- | --- | --- |
| **IDE 底座** | **Tauri v2 自建薄壳** | 真桌面 + 系统 webview（不捆绑 Chromium，60–150MB）+ updater/notification/移动端/capability 权限全齐；约 1–2 万行、无上游依赖、P0 1 周可闭环 |
| **Agent 运行时** | **AgentRuntimeAdapter 可插拔**：claude / codex / opencode / reasonix / zcode… | 交互 TUI + headless 双模式；每 CLI 一个适配文件；见 §6 |
| **编辑器** | **CodeMirror 6**（默认），Monaco 可替换 | 轻量（~300–500KB）、webview 稳、Vite 零配置；`EditorProvider` 抽象兜底；见 §7 |
| **工作区** | **contexts 目录模型**（无 worktree/branch 隔离） | per-agent 子目录 + git 兜底；见 2.2 |
| **编排** | **PATH shim `capilot` 命令 + Unix socket** | master 在任何 runtime 的 shell 里可用；不依赖 CLI 内部格式；见 §5 |
| **ESP 传输** | **蓝牙 BLE-NUS 默认优先** + USB CDC + WiFi WS | `EspTransport` 三实现；BLE 免线缆、低功耗、够遥测+低码率音频；见 §8 |
| **语音** | **Opus → sherpa-onnx 流式 STT → 实时文字**（无波形、不过 webview） | 中文流式识别质量好、边听边出字、免 C++ 构建链、预编译二进制；回复文本帧下行，ESP 侧 TTS 播报；见 §9 |
| **进程模型** | 托盘常驻 + 单实例锁 | 关窗不杀 worker；见 3.5 |
| **安全** | capabilities 权限 + CSP + 路径白名单 + 本地 WS/BLE token | 见 2.4 / §12 |

### 1.2 借鉴参考

交互设计借鉴 Reasonix 的成熟模式：会话 tab、状态栏 token/cost 指标、审批模式、workbench 布局。不 fork 代码——Tauri 自建更薄、约束更少。

---

## 2. 总体架构

### 2.1 仓库形态

独立仓库 `git@github.com:hachi7574/CaPilot-Ide.git`，本地位于 `~/Project/CaPilot-Ide/`，monorepo：

```
capilot-ide/
├─ src/             Rust 核心（Tauri 命令/PTY/编排/ESP/资源/STT）
├─ ui/              React + Vite 前端（CM6 / xterm / Composer）
├─ capabilities/    Tauri v2 权限声明
├─ tauri.conf.json  窗口/打包/更新/CSP
└─ docs/            本开发计划与架构决策
```

ESP 固件在 CaPilot 主仓库 `Firmware/PlatformIO/` 维护，经 ESP bridge 与 IDE 通信（不混进 IDE 仓库）。

### 2.2 模块划分

```
capilot-ide/
├─ src/ (Rust)
│  ├─ agent_runtime/
│  │  ├─ adapter.rs       AgentRuntimeAdapter trait（见 §6）
│  │  ├─ pty.rs           PTY 会话管理（portable-pty）
│  │  └─ runtimes/        claude.rs / codex.rs / opencode.rs / reasonix.rs / zcode.rs
│  ├─ orchestration/
│  │  ├─ session.rs       会话模型、状态机
│  │  ├─ dispatcher.rs    PATH shim 接收、worker 派发、指令注入
│  │  └─ smart_return.rs  智能返回分级
│  ├─ editor_fs/
│  │  ├─ workspace.rs     路径白名单、文件 CRUD
│  │  ├─ watch.rs         notify → 前端事件
│  │  └─ git.rs           git CLI 封装
│  ├─ esp/
│  │  ├─ transport.rs     EspTransport trait（见 §8）
│  │  ├─ usb.rs / wifi.rs / ble.rs
│  │  ├─ protocol.rs      帧编解码
│  │  └─ audio.rs         Opus 解码、播放
│  ├─ voice/
│  │  └─ stt.rs           sherpa-onnx 流式 STT
│  ├─ resource.rs         CPU/内存采集（Win Job Object / Unix 进程树）
│  ├─ persistence.rs      sqlite（会话/设置/草稿/汇报）
│  └─ settings.rs
└─ ui/ (React)
   ├─ components/
   │  ├─ editor/          CM6 封装、autosave
   │  ├─ terminal/        xterm 封装、Channel 流
   │  ├─ composer/        输入区 + 功能区（见 §4）
   │  ├─ leftbar/         左栏 3 区：品牌→操作栏（4 键：👁☰📁+⚙）→项目/终端树（置顶可折叠 Master）
   │  ├─ rightbar/        右栏上下分屏：上半部 3 Tab（概览仪表盘/文件树/Git）/ 下半部 Master Report
   │  ├─ resource/        资源曲线弹层
   │  ├─ statusbar/       ESP/电量/汇报/worker/模式
   │  └─ onboarding/      首次引导
   └─ state/              zustand（agent 状态、composer、设置）
```

contexts 目录模型：

```
~/CaPilot/workspaces/<项目>/
├─ context/              # 共享上下文（README、规则、设计稿）
├─ agents/
│  └─ <agent-id>/        # 每 agent 工作区（PTY cwd）
│     └─ .agent-meta.json  # role / runtime / resume_key / 状态
└─ sessions.db           # sqlite
```

### 2.3 关键数据流

```
【状态流】 PTY/headless 输出 → agent_runtime → event → 前端 tab/状态点
【指令流】 用户(composer/语音文字) → pty_write → agent PTY
          master 编排：capilot dispatch → shim → Unix socket → 编排器 → worker PTY/headless
【音频流】 ESP mic → Opus → transport → Rust 解码 → sherpa-onnx STT → 实时文字 → composer
          回复 → 文本帧下行 → ESP TTS 播报（音频不过 webview）
【资源流】 agent pid/Job Object → resource → 状态栏 + 弹层
```

### 2.4 技术选型表

| 层 | 选型 | 理由 |
| --- | --- | --- |
| 壳 | **Tauri v2** + wry（系统 webview） | 真桌面、不捆绑 Chromium、生态齐全 |
| 前端 | React 18 + TS + Vite | 生态最大 |
| 编辑器 | **CodeMirror 6** | 轻量（~300–500KB）、webview 稳、Vite 零配置；Monaco 可替换 |
| 终端 | xterm.js + addon-fit | 渲染 CLI TUI |
| PTY | `portable-pty`（Rust） | Win=ConPTY / mac、Linux=PTY，wezterm 同款 |
| 串口/BLE/WS | `serialport` / `btleplug` / `tokio-tungstenite` | BLE 走 NUS GATT（默认优先） |
| 资源 | `sysinfo` + Windows Job Object | 整棵进程树统计 |
| 文件监视 | `notify` | 外部/agent 改动推前端 |
| 持久化 | `rusqlite` | 会话/草稿/汇报 |
| 音频 | `cpal` + `rodio`（Rust 侧）+ `opus` crate | 全程不过 webview |
| STT | **sherpa-onnx**（流式中文模型） | 中文质量 + 实时出字 + 免 C++ 构建链 |
| git | 调 git CLI | 解析 `git status --porcelain` 等，不引 libgit2 |
| Tauri 插件 | updater / notification / dialog / store / log / tray-icon / single-instance | 产品化必备 |
| 状态 | zustand | 轻量 |

---

## 3. 页面布局（Tauri 桌面应用）

> 布局形态为**"终端为中心的 agent 工作台 + Composer 智能输入层"**，主区分三层：顶部标签栏（固定高度）、中间内容区（可拆分）、底部输入区。每个 agent 标签页以 xterm 终端为主体（渲染该 CLI 自己的 TUI，能力最全、零抽象损耗），Composer 作为统一智能输入层停靠底部。

### 3.1 整体布局

```
┌─ 标题栏（窗口拖拽 / 托盘常驻）──────────────────────────────────────────────────────────┐
├───────────┬──────────────────────────────────────────────────────────┬──────────────────┤
│ 左侧边栏   │  主区（三层）                                             │ 右栏              │
│ ←拖拽→    │                                            ←拖拽→         │                  │
│           │                                                          │ ┌─Tabs─────────┐ │
│ CaPilot   │ ┌─ 标签栏（固定高度）──────────────────────────────────────┐│ │∿概览📄文件 ⚒Git│ │
│───────────│ │ [☰][⭐master●][🤖worker-1 claude◉][editor: main.rs][+] ││ ├──────────────┤ │
│[👁][☰]   │ ├────────────────────────────────────────────────────────┤│ │              │ │
│[📁+][⚙]  │ │                                                    │ ││ │ 概览仪表盘    │ │
│           │ │  内容区（可拖拽标签拆分：左右 / 上下）                     │ ││ │ （可逐区折叠） │ │
│⭐Master[▲]│ │                                                    │ ││ │ 文件树/Git    │ │
│ ├─ ✅ hi │ │  ┌─────────────────┬─────────────────┐               │ ││ │              │ │
│📁CaPilot[▼]│ │  │ xterm 终端#1   │ xterm 终端#2    │               │ ││ │▸ 📁 Project  │ │
│ ├─ 🔄 hi │ │  │ (agent TUI)    │ (agent TUI)     │               │ ││ │▾ 📁 Workspace│ │
│ └─ 🤖 w1│ │  │                │                 │               │ ││ │  📄File-1   │ │
│📁Global[▼]│ │  └─────────────────┴─────────────────┘               │ ││ │  🌐File-2   │ │
│ └─ 💤 hi │ │                                                    │ │├──────────────┤ │
│           │ ├────────────────────────────────────────────────────────┤│←拖拽→        │ │
│           │ │  输入区（默认展开 · 拖拽拉高 160px~60vh）                 ││              │ │
│           │ │  → agent: worker-1  发消息…（/命令·@文件·!终端·拖入）     ││ 🤖 Master     │ │
│           │ │  [+文件/引用/skill↑][模型↑][速度↑][🤖worker][Ask|Auto|Yolo] ││ Report        │ │
│           │ │                                                          ││ (始终可见)    │ │
└───────────┴──────────────────────────────────────────────────────────┴──────────────────┘
  状态栏：🔵BT/USB/📶WiFi · 电量 78% · 汇报🔔开 · worker×2 · 模式[Ask] · 速度 auto
```

### 3.1.1 标签栏（固定高度）

```
┌──────────────────────────────────────────────────────────┐
│ [☰] [⭐master●] [🤖worker-1 claude◉] [editor: main.rs] [+] │
└──────────────────────────────────────────────────────────┘
```

- **`[☰]`**：左侧边栏折叠/展开按钮
- **标签**：agent 终端标签与编辑器文件标签（CM6）混排，按打开顺序排列；**标签栏只显示当前在左侧栏选中的项目所打开的终端和文件**（切换项目时标签栏同步刷新）
- agent 标签带角色徽标（⭐master / 🤖worker）+ runtime 徽标（claude/codex/opencode…）+ 状态点（idle 灰 / running 蓝·脉冲 / waiting_input 黄 / busy 橙 / done 绿 / failed 红）；长度溢出时压缩宽度 + 悬浮全名；右键菜单：关闭 / 关闭其他 / 关闭右侧 / 关闭全部
- **`[+]`**：新建终端标签——弹出 runtime 选择菜单（未安装/未登录置灰），点击后新建 agent 标签并聚焦，新终端归属当前选中的项目
- 点击标签切换到对应内容区面板

### 3.1.2 内容区（可拆分）

- 默认单面板：当前选中标签的 xterm 终端（agent CLI TUI）或 CM6 编辑器（打开文件时）
- **拖拽标签拆分**：将标签拖到内容区边缘 → 左右拆分（拖到左/右边缘）或上下拆分（拖到上/下边缘），产生并排面板；每面板独立显示一个标签的内容
- 面板间分割线可拖拽调整比例
- 打开编辑器文件时，该标签内容区切换为 CM6 编辑器（同一标签，内容切换，非新标签）

### 3.1.3 输入区

- 全局单一输入区，停靠在主区底部（非每 tab 独立）
- 发送目标由当前选中的标签决定（选中 agent 标签 → 发到该 agent PTY；选中 master 标签 → 发到 master PTY）
- `Tab` 键切换发送目标（agent ⇄ master），composer 左侧小徽标指示当前目标
- 详细交互规格见 §3.2 / §4

### 3.2 Composer（输入区）

全局单一输入区，停靠在主区底部，不受内容区面板拆分影响。

```
┌─ 输入区（默认展开 · 拖拽拉高 160px~60vh）────────────────────────────────┐
│ → agent: worker-1  给 cli/agent/worker/master 发消息…                  │
│ （/ 命令 · @ 文件 · ! 终端 · 拖入文件 · 语音转写文字实时打入）             │
├────────────────────────────────────────────────────────────────────────┤
│ [+文件/引用/命令/skill↑] [模型↑] [速度↑(high/mid/fast/auto)]           │
│ [🤖worker 开|关] [[Ask]|Auto|Yolo]                          [▼/▲]     │ ← 固定行高
└────────────────────────────────────────────────────────────────────────┘
```

- **发送目标指示**：输入区左侧显示 `→ agent: <id>` 或 `→ master`，由当前选中标签决定；`Tab` 切换发送目标（agent ⇄ master）
- **输入区**：`Enter` 发送 / `Shift+Enter` 换行；Ctrl+Z/Y/C/V/X/A（textarea 原生）；`↑/↓` 草稿历史；`@` 文件补全 chip；`!命令` 直发终端（绕过 agent 会话）；语音转写文字实时打入；拖文件入输入区 → `@路径` chip
- **功能区（固定行高，收起时不隐藏）**：`[+]` 附件菜单（文件/引用/命令/skill）、`[模型↑]` 模型切换（runtime 提供列表）、`[速度↑]` high/mid/fast/auto（映射各 runtime 的 thinking effort）、`[🤖worker 开|关]` 当前标签是否作为 worker 加入编排池、`[[Ask]|Auto|Yolo]` 权限模式分段开关
- **键盘语义（composer 聚焦时）**：`Tab` = 切换发送目标（agent ⇄ master）；`Shift+Tab` = 循环 `[[Ask]|Auto|Yolo]` 模式；`Esc` 无效；不使用 Tab 缩进（缩进走 Shift+Tab 或代码块）
- **收起/展开**：`[▼]` 只收起输入区、功能区保持可见，按键变 `[▲]`；草稿独立保存，收起/重启不丢
- **语音文字写入**：语音转写文字实时追加进输入区；转写中允许手动编辑；VAD 停顿后停止，文字停在框内待确认发送；语音开始 → composer 自动展开

### 3.3 左侧边栏与右栏

左侧边栏（可折叠）分 3 个纵向区域：

#### Zone 1 — 品牌标识（不可点击）

```
CaPilot
```

纯标识，无交互。

#### Zone 2 — 操作栏（四按键居中均等分布）

```
    [👁]       [☰]       [📁+]       [⚙]
```

- **`[👁]`**：worker 显示过滤——单击循环：👁 全部显示 → 👁‍🗨 仅显示 worker → 🚫 隐藏 worker
- **`[☰]`**：对全部项目统一收起 / 恢复之前展开的项目
- **`[📁+]`**：新建项目按钮
- **`[⚙]`**：设置入口（runtime 管理 / ESP 配对 / 通用偏好）

#### Zone 3 — Projects 区域

置顶的 Master 会话（可折叠）+ 项目终端树。**主区标签栏只显示当前选中项目下的终端和文件**——双击项目标题行"聚焦"该项目（左侧栏高亮该项目的背景、主区标签栏切换为该项目的标签集合）。

```
⭐ Master 会话                        [▲]

📁 CaPilot                            [▼]
 ├─ ✅ IDE搭建讨论                  3小时
 ├─ 🔄 BLE调试                     20分钟
 └─ 🤖 worker-1                    5分钟

📁 Global                             [▼]
 ├─ 🔄 hi                           1小时
 └─ 💤 临时终端                     4小时
```

- **⭐ Master 会话**：固定在项目列表最顶部，不可删除/移动；**单击标题可收起/展开**（与普通项目行为一致）；展开显示其下的终端标签；**位置永远在第一个**
- **项目标题行**：`📁 项目名` + 右侧 `[▼]/[▲]` 折叠箭头；**单击项目名 → 收起/展开该项目下所有终端标签**
- **双击项目名** → "聚焦"该项目：左侧栏高亮该项目背景，主区标签栏同步切换为仅该项目下的终端和文件标签
- **终端标签行**：`【状态图标】终端名` + 右侧**最近消息时间**（如 "刚刚"、"3小时"），缩进挂在所属项目下
  - 点击终端标签 → 主区标签栏切换到对应标签并聚焦
  - 状态图标：✅ 已完成 / 🔄 进行中 / 💤 休眠中（普通终端） / 🤖 worker
- **项目支持拖拽排序**（默认按创建时间排列；Master 始终第一）

#### 标签栏项目过滤规则

- 主区标签栏只显示**当前左侧栏聚焦的项目**下的终端和文件标签
- 默认聚焦最后操作的项目；未聚焦任何项目时显示全部标签
- 新建终端（`[+]`）默认归属到当前聚焦的项目
- 切换项目聚焦时，标签栏标签集合同步刷新；当前打开的终端/文件标签不关闭（保留在后台），切回该项目时恢复

**Agent 状态图标：**

| 图标 | 状态 | 含义 |
| --- | --- | --- |
| ✅ | 已完成 | agent 任务执行完毕（done 状态） |
| 🔄 | 进行中 | agent 正在运行（running/busy 状态） |
| 💤 | 休眠中 | 普通终端，未运行任何 agent（idle） |
| 🤖 | worker | 该 agent 角色为 worker |

**项目右键菜单：**

```
┌──────────────────────────────┐
│ 📌 置顶项目                   │
│ 🌿 创建隔离的交付工作区       │
│ 🖥 新建终端                   │
│ 📁 在文件管理器中显示         │
│ ✏ 重命名项目                 │
│ 📦 归档对话                   │
│──────────────────────────────│
│ 🗑 移除                       │
└──────────────────────────────┘
```

- **置顶项目**：项目固定在列表顶部（位于 Master 会话之下）
- **创建隔离的交付工作区**：基于当前项目创建一个独立 context 环境
- **新建终端**：在该项目下新建 agent 终端（弹出 runtime 选择）
- **在文件管理器中显示**：用系统文件管理器打开项目目录
- **重命名项目**：修改项目显示名称
- **归档对话**：将该项目下所有会话打包归档（sqlite + 日志），从列表中移除但不删除磁盘文件
- **移除**：从列表中删除该项目（不删除磁盘文件，可重新添加）

**终端标签右键菜单：**

```
┌────────────────────────────────────────┐
│ 🤖 设置为 worker                        │
│ 📌 在此项目中置顶此终端                 │
│ ✏ 重命名此终端                         │
│ 🗑 结束并删除此终端                     │
└────────────────────────────────────────┘
```

- **设置为 worker**：如果尚未启动 agent → 自动启动并设为 worker；已在运行 → 直接切换
- **置顶此终端**：该终端在其项目下排到最前
- **重命名此终端**：修改终端标签显示名称（不改 agent id）
- **结束并删除此终端**：kill PTY（SIGTERM → SIGKILL），从项目列表中移除

- 左侧栏、主区、右侧栏之间的分割线可拖拽改变各区域宽度
- 右侧栏**不可隐藏**（始终可见），分为上下两部分，中间分割线可拖拽调整比例

#### 右栏上半部 — 3 个 Tab

Tab 切换：`∿ 概览` | `📄 文件` | `⚒ Git`

##### Tab 1 — `∿ 概览`

显示当前选中会话的运行时概览，**每区可单击收起/展开**（收起时只显示一行摘要）。

```
┌──────────────────────────────────────────────┐
│ 📊 Runtime                          [▼/▲]    │
├──────────────────────────────────────────────┤
│ 上下文窗口                                   │
│ 🟢 上下文充足              42,134 / 1,000,000│
│ 已用 42k Tokens   距离压缩 757k Tokens      │
├──────────────────────────────────────────────┤
│ 会话指标                          [▼/▲]      │
│ 缓存命中率 79.54%  运行时间 20m22s          │
│ 请求次数 18        累计 Tokens 568,822       │
├──────────────────────────────────────────────┤
│ 用量分析                          [▼/▲]      │
│ 主模型 ████████░░░░░░░ 28%                  │
│   157,278 Tokens  Cache 82.39%               │
│ 子代理 ██████████████░░ 72%                  │
│   411,544 Tokens  Cache 78.44%               │
├──────────────────────────────────────────────┤
│ 剩余用量                          [▼/▲]      │
│ ⏱ 5小时窗口  ███████████░░░░ 剩余 72%       │
│ 📅 周额度    █████████░░░░░░ 剩余 56%       │
│ 🗓 月额度    █████████████░░ 剩余 82%       │
├──────────────────────────────────────────────┤
│ 💻 Computer Status                [▼/▲]      │
│ 🟢 Online                                    │
│ CPU 35%   Memory 12.4GB/32GB  GPU 42%       │
│ Disk 68%  Network ↑12Mbps ↓85Mbps           │
├──────────────────────────────────────────────┤
│ 🔌 ESP Device Status              [▼/▲]      │
│ 🟢 ESP32-C6  🔵 Bluetooth  🔋 78%          │
│ Temp 42℃  Signal -55dBm  FW v1.2.0         │
└──────────────────────────────────────────────┘
```

各区详情：

| 区 | 内容 | 数据来源 |
| --- | --- | --- |
| **📊 Runtime** | 上下文窗口用量（已用/上限 + 进度条 + 距压缩估算）、会话指标（缓存命中率、运行时间、请求次数、累计 tokens） | agent PTY 输出解析 + runtime `--help` 容错提取 |
| **用量分析** | 主模型/子代理 token 占比（进度条 + 绝对值 + 各自缓存命中率） | 同 runtime 输出解析 |
| **剩余用量** | 5 小时窗口、周额度、月额度的剩余百分比进度条 | runtime API 或 CLI 查询 |
| **💻 Computer** | 本机状态：在线指示、CPU/内存/GPU/磁盘/网络速率 | Rust `sysinfo` + 系统 API |
| **🔌 ESP** | ESP 连接状态、连接方式、电量（进度条）、温度、信号强度、固件版本 | `EspTransport` 遥测帧 |

收起规则：每区标题行右侧 `[▼/▲]` 切换；收起后只显示一行摘要（如 `📊 Runtime  42k/1M Tokens · 缓存79.5% · 运行20m`）。

#### 右栏 Tab 2 — `📄 文件`

- 文件树（contexts 目录），顶部搜索栏过滤
- 支持展开/折叠、点击在编辑器打开、拖拽到 composer/终端

#### 右栏 Tab 3 — `⚒ Git`

- Git 源代码控制面板
- 列出当前项目所有改动文件（`git status --porcelain`），显示 +/- 行数
- 点开在编辑器以 diff 视图查看
- 支持 stage/unstage（`git add`/`git reset`）、commit（输入 commit message → `git commit`）
- 分支切换、pull/push 快捷操作

#### 右栏下半部 — Master Agent Report（始终可见，独立于 Tab）

```
┌──────────────────────────────────────────────┐
│ 🤖 Master Agent Report              [▼/▲]    │
├──────────────────────────────────────────────┤
│ 20s ago                                     │
│                                              │
│ "ESP32 OTA升级完成，等待下一步任务"            │
│                                              │
│ Task: OTA Firmware Update                    │
│ Status: Waiting                              │
│                                              │
│ [展开任务详情]  [跳转到 Master 终端]           │
└──────────────────────────────────────────────┘
```

- **始终可见**：不受上半部 Tab 切换影响，固定显示在右栏底部
- 与上半部之间分割线可拖拽调整上下比例
- 内容：master agent 最近一次汇报摘要 + 任务名 + 状态 + 距上次更新时间
- 可收起（`[▼/▲]`），收起后只显示单行摘要（`🤖 Master Report  20s ago · "OTA升级完成" · Waiting`）
- **`[展开任务详情]`**：在主区打开完整汇报内容
- **`[跳转到 Master 终端]`**：主区切换到 master agent 标签
- 数据来源：编排器 `smart_return` 推送

### 3.4 底部状态栏

| 指示器 | 说明 |
| --- | --- |
| **ESP 连接** | 🔵蓝牙 / 🔌USB / 📶WiFi 图标，断连闪烁提示（蓝牙为默认优先链路） |
| **ESP 电量** | 电池百分比图标（USB 供电时隐藏） |
| **master 汇报开关** | 智能返回开/关，点击切换 |
| **worker 计数** | 当前编排池内 worker 数量 |
| **模式 / 速度** | 当前 `[Ask\|Auto\|Yolo]` 档位与模型速度（Composer 收起时仍可见） |
| **语音状态** | 语音输入占用指示 |

### 3.5 浮层与常驻

- **资源监视器**：弹窗/浮层按需展开（每 agent CPU/内存曲线），不占用常驻面板
- **ESP 详情 / 配对向导**：浮层，含蓝牙配对、WiFi 配网、USB 枚举流程
- **托盘常驻**：关窗最小化到托盘，worker 继续运行，系统通知唤醒；**单实例锁**防止多开抢占 PTY/串口/BLE

### 3.6 布局约束

- 无 worktree/branch 隔离：agent 工作区为 contexts 目录下的 per-agent 子目录（见 2.2）
- 编辑器默认 CodeMirror 6；`EditorProvider` 抽象保留替换为 Monaco 的能力
- 布局全部自建，改动自由，但保持**"标签栏（固定高）+ 内容区（可拆分）+ 输入区（底部）"**三层主形态与状态栏指示器不变
- 文件树位于右栏 `📄 文件` tab；左侧栏为 3 区：品牌 → 操作栏（四按键居中含 `⚙`）→ 项目/终端树（置顶可折叠 Master + 项目聚焦 → 标签栏过滤）
- 左侧栏/主区/右侧栏之间分割线可拖拽改变宽度；右侧栏始终可见，内部分上下两部分（上半部 3 Tab + 下半部 Master Report），中间分割线可拖拽

---

## 4. 对话/终端交互规格

### 4.1 原则：终端是"真相"，输入框是"智能键盘"

- **终端为主**：每个 CLI 的 TUI 是能力最全形态（斜杠命令、模型切换、MCP、权限审批都在里面），透传 = 零抽象损耗
- **输入框为辅**：承接三件"终端里做不好"的事——语音转写文字（可编辑后发送）、文件拖拽成 `@路径` chip、master 编排指令
- 两者写进**同一条 PTY**，不搞两套会话状态：输入框发送 = 等价于终端里敲文本 + 回车

### 4.2 交互流

| # | 交互 | 行为 |
| --- | --- | --- |
| ① | 直接使用 | 点进终端手打，走 CLI 原生 TUI |
| ② | 输入框发送 | `Enter` 发送（`pty_write(文本+\r)`）；斜杠命令透传 |
| ③ | 拖文件 → 输入框 | 应用内/OS 拖入 → `@路径` chip → 按 runtime mention 语法写入（不识别则引号包裹） |
| ④ | 拖文件 → 终端 | 路径文本 shell 转义后粘贴进 PTY |
| ⑤ | 语音输入 | ESP/PC 麦 → Opus → 流式 STT → **文字实时打进输入框**（边听边出字）→ VAD 停顿停止 → 可编辑后发送 |
| ⑥ | master 编排 | master 敲 `capilot dispatch worker-2 "任务"` → shim → 编排器 → 注入 worker PTY/headless |

### 4.3 快捷键总表

| 键 | 行为 |
| --- | --- |
| `Enter` | 发送（到当前发送目标） |
| `Shift+Enter` | 换行 |
| `Ctrl+Z` / `Ctrl+Y` | 撤销 / 重做（textarea 原生） |
| `Ctrl+C` / `Ctrl+V` / `Ctrl+X` / `Ctrl+A` | 复制 / 粘贴 / 剪切 / 全选（textarea 原生） |
| `↑` / `↓` | 草稿历史（最近 N 条） |
| `Tab` | 切换发送目标：agent 终端 ⇄ master 终端（composer 聚焦时） |
| `Shift+Tab` | 循环 `[[Ask]\|Auto\|Yolo]` 模式 |
| `Esc` | **无效**（不响应） |
| `@` | 文件路径补全 → chip |
| `!` | `!命令` 直发终端（绕过 agent 会话） |
| 拖拽 | 入输入区 → chip；入终端区 → 路径粘贴；OS 拖入窗口 → 同管线 |

### 4.4 焦点与收起规则

```
收起时机：手动点 [▼]（仅收起输入区，功能区保持）
展开时机：应用启动（默认展开）；拖文件进窗口；语音开始转写；点击 [▲]
焦点环：Composer ⇄ 终端(xterm) ⇄ 文件树/编辑器
```

- xterm 内 `Tab` 不被劫持（Tab 导航只在 composer 聚焦时激活）
- 终端获得焦点不自动收 composer（收起仅由 `[▼]` 触发），避免打断操作

### 4.5 状态机与状态点

```
idle(灰) → running(蓝·脉冲) → waiting_input(黄) / busy(橙)
   ↕ resume                         ↓ 完成/失败
   done(绿) / failed(红)
```

- running/busy 区分：headless 任务由 Rust 直接跟踪；交互会话用启发式（进程存活 + 输出特征），**UI 标注"仅供参考"，以终端画面为准**
- 完成/失败 → 状态点变色 + 系统通知（worker 完成 / ESP 断连 / 汇报就绪）
- 关窗最小化到托盘后状态点持续更新，回来一眼看到谁干完了

### 4.6 worker 锁定与冲突处理

- 设置 worker 的途径：composer `[🤖worker 开|关]` 功能区按键 / 左侧栏终端标签右键菜单「设置为 worker」
- worker 开关打开 → 角色 `role: worker`，进编排池，**输入框默认锁定**（只读，防编排冲突），可手动解锁（🔓）
- 如果该终端尚未启动 agent → 自动启动 agent 并设为 worker；如果 agent 已在运行 → 直接切换角色
- 锁定期间用户在终端手打 → 不静默拦截，提示"此 agent 是 worker，输入会被编排结果覆盖"，给"仍然发送/解锁"二选一
- master tab 无此开关（固定 master）

### 4.7 语音文字写入

- 语音转写文字实时追加进**当前焦点 composer**；转写中允许手动编辑；VAD 停顿后停止，文字停在框内待确认发送
- 语音开始 → composer 自动展开；无 composer 语音按键（触发走全局快捷键 / ESP 硬件按钮）

### 4.8 runtime 切换（发送目标与引擎）

```
当前 tab 顶部 runtime 选择器 [claude ▾]
  → kill 旧 PTY（SIGTERM→SIGKILL，清僵尸）
  → 同一 context 目录 spawn 所选 runtime 的 PTY
  → 按该 runtime 的 resume key 恢复会话历史
```

- 会话历史按 `(tab, runtime)` 隔离，切换互不串
- 一个 tab = 一个工作位；想同时用 claude + codex → 新建 tab
- 新建面板 `is_available()` 探测：装了亮 / 没装置灰 + 引导链接 / 未登录标"未登录"

---

## 5. master/worker 模型

### 5.1 角色

- **worker**：`role: worker`（composer `[🤖worker 开|关]` 或 agent 详情切换），进编排池，工具范围默认限定自身 agents 目录（沿用 PRD I7 权限沙箱）
- **独立 agent**：默认角色，master 看不到、管不到，仅用户直管
- **master agent**：固定 master 会话（不可删除/重命名），其终端标签位于主区 tab 栏；左侧栏可通过 `[👁]` 过滤按钮查看所有 worker（`👁‍🗨` 模式）

### 5.2 PATH shim 编排（核心机制）

不依赖任何 CLI 内部格式的"后通道"：给 master 会话的 PATH 前缀一个真实可执行文件 `capilot`（shim），master 在任何 runtime 的 shell 里直接敲：

```
capilot dispatch worker-1 "跑回归测试"   → 派发（注入 worker PTY / headless 启动）
capilot status                          → 列出 worker 状态
capilot report "摘要"                    → 提交智能返回
```

shim 经 **Unix socket** 直连 Rust 编排器，可靠一个量级，且 claude/codex/opencode/reasonix 统一可用——这正是"运行时可替换"在编排层的落实。

### 5.3 编排器（Rust `orchestration/dispatcher.rs`）

- 维护 worker 池（role=worker 的会话 + 状态）
- `dispatch`：选 idle worker → 交互模式 `pty_write(指令+\r)` / headless 模式 `spawn_headless` → 置 busy
- 完成事件 → 汇报聚合 → master 会话收"worker X 完成：摘要"
- worker 取消标记 → 立即移出 worker 池

### 5.4 智能返回

- 规则引擎（`orchestration/smart_return.rs`）监听 worker 完成事件，按 PRD 附录规则表分级：失败→完整输出+错误摘要；≤600 字符→完整；600~3000→概述+关键文件；>3000→标题级
- **约束**：只影响 master 向用户的汇报粒度，worker 终端原始输出永不失
- **开关**：设置项「master 智能返回」（默认开）；关闭后始终回完整输出
- **呈现**：master 会话流内分级摘要；ESP 端走语音+文字

### 5.5 角色切换 UI

- IDE：composer `[🤖worker 开|关]` 功能区按键 / 左侧栏终端标签右键「设置为 worker」（即时生效，写 `.agent-meta.json`）
- ESP：列表模式长按 agent 标签 →「设为 worker / 取消」→ 与 IDE 实时同步

---

## 6. agent 运行时（可替换）

### 6.1 AgentRuntimeAdapter（Rust trait）

```rust
trait AgentRuntimeAdapter {
    fn id(&self) -> &str;                                  // "claude" | "codex" | ...
    fn is_available(&self) -> bool;                        // CLI 已安装？
    fn is_authenticated(&self) -> bool;                    // 登录态？（未登录时提前显示，避免神秘报错）
    fn list_models(&self) -> Vec<ModelInfo>;               // 模型列表（composer [模型↑]）
    fn spawn_interactive(&self, s: &AgentSession) -> Result<PTYHandle>;   // 交互 TUI
    fn spawn_headless(&self, s: &AgentSession, prompt: &str)
        -> Result<HeadlessRun>;                            // 结构化一次性任务
    fn resume(&self, s: &AgentSession) -> Result<PTYHandle>;              // 续会话
    fn speed(&self, s: &AgentSession, v: Speed) -> Vec<String>;           // 速度→启动/注入参数
    fn mode(&self, s: &AgentSession, m: PermissionMode) -> Vec<String>;   // Ask/Auto/Yolo→注入命令
}
struct PTYHandle { write, resize, kill, on_data, exit_code }
struct AgentSession { id, runtime, mode, cwd, context_dir, role }
```

### 6.2 runtimes/ 每 CLI 一文件

| runtime | 交互 | headless | resume | 备注 |
| --- | --- | --- | --- | --- |
| `claude.rs` | `claude`（TUI） | `claude -p --output-format stream-json` | `claude --resume` | |
| `codex.rs` | `codex` | `codex exec` | `codex resume` | |
| `opencode.rs` | `opencode` | `opencode run` | `opencode --continue` | 多 provider 运行时 |
| `reasonix.rs` | `reasonix` | `reasonix run --events-jsonl` | `reasonix -c` | |
| `zcode.rs` | `zcode` | 按其 `--help` 填写 | 同左 | 接入时以实际 CLI 为准 |

- **新增一个 CLI = 加一个文件**：这是"运行时可替换"的扩展点
- 参数表按各 CLI `--help` 容错解析（版本漂移防御）
- 速度（high/mid/fast/auto）与权限模式（Ask/Auto/Yolo）在各 runtime 文件内维护映射表，不支持的档置灰

### 6.3 会话与生命周期

- PTY 管理（`agent_runtime/pty.rs`）：spawn / 流式转发（Channel 二进制帧）/ 输入 / resize / 退出清理（SIGTERM→SIGKILL 防僵尸）
- headless 会话渲染为 xterm 面板（复用同一 PTY/Channel 通道），Rust 解析结构化输出跟踪状态
- 元数据 `runtime` + `role` 写 `.agent-meta.json`，sqlite 存会话与 resume_key

---

## 7. 编辑器与文件系统

### 7.1 CodeMirror 6

- CM6 体积 ~300–500KB、webview（含 WebKitGTK）表现稳、Vite 零配置
- 多 agent 目录用多 model 天然支持；语言高亮按需装 `@codemirror/language-data`
- `EditorProvider` 抽象：未来如需 Monaco 专属能力（peek/语义 token 等）可替换，前端其余代码不动

### 7.2 文件系统 API 与白名单

- 前端不接触文件系统，全部经 Rust invoke：`fs_read` / `fs_write` / `fs_list` / `fs_watch`
- **路径白名单**：必须落在 workspace 根内，拒绝 `..` 逃逸；中文/空格路径注入 PTY 前 shell 转义
- autosave：`onDidChangeModel` → debounce 800ms → 写盘
- 外部/agent 改动：`notify` → event → 对应 model 刷新/提示

### 7.3 文件树与拖拽

- 右栏 `📄 文件` tab：文件树（contexts 目录），顶部搜索栏，支持展开/折叠、多选、拖拽
- 拖拽双通道：入输入框 → `@路径` chip；入终端区 → 路径粘贴；OS 拖入窗口 → `onDragDropEvent` → 同一管线（落 workspace 内转相对路径，否则提示）

### 7.4 Git 源代码控制面板（右栏 `⚒ Git` tab）

- 列出当前项目所有改动文件（`git status --porcelain`），显示 +/- 行数与改动类型（M/A/D/R）
- 点开文件在编辑器以 diff 视图查看
- 支持 stage/unstage（`git add`/`git reset`）、commit（输入 commit message → `git commit`）
- 分支切换、pull/push 快捷操作

### 7.5 git 封装

- 调 git CLI 解析 `git status --porcelain` 等，简单可靠

---

## 8. ESP 硬件集成

### 8.1 EspTransport（三实现，BLE 默认优先）

```rust
trait EspTransport { connect / read / write / status / kind }
├─ UsbSerial   （serialport，USB CDC，供电+通信一体）
├─ WifiWs      （tokio-tungstenite 内嵌 WS server，127.0.0.1:8789 + token）
└─ BleUart     （btleplug，BLE UART/NUS GATT）   ← 默认优先（可能是主要连接方式）
```

- 控制/遥测/电池/状态走 BLE 数据通道（~10–30KB/s 实测吞吐，够遥测与低码率数据）
- 音频走 Opus 低码率（16kbps ≈ 2KB/s，余量 6 倍）同通道
- 平台注意：Win 打包加 Bluetooth capability manifest、mac 加蓝牙 entitlement、Linux 需 BlueZ 运行

### 8.2 协议（沿用 PRD H6，修订）

- 帧：`magic + type + len + payload + crc16` + **seq + version**
- 控制帧：ack/重试（BLE 管道非可靠，需确认）；音频帧：fire-and-forget
- IDE → ESP：`agent_update / audio_start / subtitle / connection_status`
- ESP → IDE：`touch_event / key_event / mode_change / audio_chunk / mic_request`
- 心跳：5s 双向 / 15s 超时提示

### 8.3 配对与连接向导

- 首次引导/ESP 详情浮层：蓝牙扫描配对、WiFi 配网（token 下发）、USB 枚举
- 用户如何把 token 告诉 ESP32 要有明确流程（入 onboarding）

### 8.4 三模式镜像

| ESP 模式 | IDE 侧对应 |
| --- | --- |
| 列表模式 | agent 状态快照推送（4 卡片/页 + 翻页 + 长按菜单） |
| 设置模式 | 共享同一份配置（WiFi/音量/亮度/mic 源/智能返回开关）双向同步 |
| master 语音 | 音频上行（Opus）+ 实时文字回显；回复文本帧下行 → ESP TTS 播报 |

### 8.5 固件协作

- ESP 固件在 CaPilot 主仓库 `Firmware/PlatformIO/`：NUS BLE 服务、Opus 编码（ESP-IDF libopus）、TTS 播报、三模式 UI（参考小智AI）
- C5 硬件（ES8311 mic/喇叭）先跑通录音/放音 demo 再联调

---

## 9. 语音链路

```
ESP mic → Opus(ESP-IDF, 16kHz/16-24kbps) → 传输(BLE 数据通道默认)
  → Rust 解码(opus crate) → sherpa-onnx 流式 STT → 实时文字 → composer 输入框
  停顿(VAD) → 停止 → 文字停框内可编辑 → 发送
回复 → 文本帧下行 → ESP TTS 播报（barge-in：播报中 PTT 打断）
```

- **无波形可视化、音频字节不过 webview**：webview 只收文字，音频大流量风险整体消除
- **STT 选 sherpa-onnx**：中文流式识别质量好（paraformer/zipformer 中文流式模型）、边听边出字、预编译二进制免 C++ 构建链、CPU 快
- mic 源：ESP 板载（默认）/ PC 麦（全局快捷键触发）
- 目标端到端 ≤3s
- composer **无语音按键**；触发走全局快捷键 / ESP 硬件按钮

---

## 10. 资源监视

- Rust 每秒 `sysinfo` 按 agent PID **整棵进程树**采集 CPU/内存 → event → 前端弹层曲线
- Windows 上父子枚举不可靠 → 用 **Job Object** 统计整组；Linux/mac 用父进程遍历
- 超限告警 → 状态栏 + 通知（可选 ESP 推送）

---

## 11. 开发步骤（分阶段）

> 每阶段可独立验证。估时为单人连续开发量。

### P0 — 最小闭环（≈1 周）

- [ ] Tauri v2 脚手架：React+Vite+TS、单实例锁、托盘常驻、窗口/布局骨架
- [ ] Rust 核心：`AgentRuntimeAdapter` trait、PTY 管理（portable-pty）、invoke/event/Channel IPC 封装
- [ ] 第一个适配器 `claude.rs`：spawn/headless/resume 参数表、`is_available`/`is_authenticated`、速度与模式映射
- [ ] CodeMirror 6：fs_read/fs_write（路径白名单）+ autosave（debounce 800ms）
- [ ] xterm.js：Channel 二进制流、resize、焦点管理（不劫持输入框快捷键）
- [ ] Composer 完整规格落地（§3.2/§4：输入区 + 功能区、快捷键、折叠、拖拽、发送目标徽标、草稿）

**验证**：双击启动 → 输入框发消息 → claude 在 xterm 响应；全部快捷键/折叠/拖拽/发送目标切换生效。

**内插 P0.5 — BLE 可行性 spike（0.5~1 天，P0 后即做）**
- [ ] 一个 ESP32 跑通 BLE NUS 遥测（电池/状态帧）→ 验证 Opus-over-BLE 吞吐（24kbps 理论余量 6 倍，实机确认）
- 理由：BLE 可能是主链路，平台坑（Win manifest/mac entitlement/BlueZ）早暴露

### P1 — 编排骨架（≈1.5–2 周）

- [ ] contexts 目录模型 + `.agent-meta.json` + sqlite 持久化 + 会话恢复（重启不丢）
- [ ] 多 tab 与角色（master 固定/worker/standalone）、runtime 切换器（kill→同 context respawn→resume）、状态机+状态点
- [ ] PATH shim `capilot`（dispatch/status/report）+ Unix socket + 编排器；worker 锁定+冲突提示
- [ ] master 面板、worker 汇报聚合、智能返回（分级 + 开关）
- [ ] 文件树 + `⚒ Git` 源码控制面板（stage/commit/分支/pull/push）

**验证**：master 派发给两个不同 runtime 的 worker 并行，完成汇报；切换 runtime 后会话可续。

### P2 — 产品化（≈1–1.5 周）

- [ ] 资源监视（Windows Job Object / Unix 进程树）+ 曲线弹层
- [ ] 系统通知（worker 完成/ESP 断连/汇报就绪）、设置页、首次引导（runtime 检测/登录态/ESP 配对向导入口）
- [ ] 自动更新（updater+签名）、三平台打包（Win 蓝牙 manifest / mac 公证 / Linux 次等支持）
- [ ] capabilities/CSP 收紧 + security-review 审查

**验证**：三平台安装包可用、更新流程走通、全新机器引导全流程。

### P3 — ESP 与语音（≈2 周）

- [ ] `EspTransport` 三实现（USB/WiFi/BLE-NUS 默认优先）+ 配对/配网向导
- [ ] 协议帧（seq+ack 控制/音频 fire-and-forget、版本化）+ 心跳
- [ ] Opus → Rust 解码 → sherpa-onnx 流式 STT → 实时文字进 composer（无波形、不过 webview）
- [ ] ESP TTS 汇报播报 + 电池/状态遥测 + 三模式镜像

**验证**：ESP32 实机 BLE 连通、语音实时出字、汇报播报通、E2E ≤3s。

---

## 12. 风险与待验证

| 风险 | 说明 | 应对 |
| --- | --- | --- |
| **Linux WebKitGTK + 重型 UI** | CM6 已缓解（轻量、稳） | 主目标 Windows（WebView2=Edge 内核）；Linux 次等支持；IPC 抽象保证壳可换 |
| **BLE 吞吐/兼容性** | NUS 数据通道实测 ~10–30KB/s；Win/mac 平台权限坑 | P0.5 spike 最早验证；协议控制帧 ack/重试 |
| **CLI 参数漂移/登录态** | claude/codex 各自更新 | 适配器对 `--help` 容错解析；`is_authenticated` 前置检测 |
| **多 agent 并发改同一文件**（无 worktree） | 已知行为 | per-agent 子目录 + `⚒ Git` 面板 + git 兜底，文档写明 |
| **音频端到端 ≤3s** | 硬指标 | P3 单独延迟测量；sherpa-onnx 流式 + 低延迟 TTS |
| **C5 硬件（ES8311）** | mic/喇叭需硬件验证 | P3 前置：先跑通录音/放音 demo |
| **master 指代解析** | 自然语言指定 worker 会指错 | master 按名确认（"你是指 XX 吗？"） |
| **Rust 生态陌生** | 编译慢、学习曲线 | 代码由 agent 编写，成本主要在编译；crate 均成熟 |

---

## 13. 里程碑汇总

| 里程碑 | 阶段 | 结果 |
| --- | --- | --- |
| M0 | P0 | 可双击启动，claude 会话在 xterm 跑通，Composer 全套交互可用 |
| M1 | P1 | master/worker 编排 + 智能返回（**可内测**） |
| M2 | P2 | 资源监视 + 产品化（更新/打包/引导/安全） |
| M3 | P3 | ESP 蓝牙语音全链路 + 发布 |

**关键路径**：P1（master/worker 编排）是差异化核心，优先保质量；P0.5（BLE spike）前置验证主链路风险。

---

## 附录 A：决策清单

| ID | 决策项 | 状态 | 结论 |
| --- | --- | --- | --- |
| D1 | IDE 底座 | ✅ 已定 | **Tauri v2 自建薄壳** |
| D2 | 编辑器 | ✅ 已定 | **CodeMirror 6** 默认，Monaco 可替换（`EditorProvider`） |
| D3 | Agent 运行时 | ✅ 已定 | **AgentRuntimeAdapter 可插拔**：claude/codex/opencode/reasonix/zcode，每 CLI 一文件 |
| D4 | 工作区 | ✅ 已定 | **contexts 目录模型**（无 worktree/branch 隔离） |
| D5 | 编排机制 | ✅ 已定 | **PATH shim `capilot` 命令 + Unix socket** |
| D6 | ESP 传输 | ✅ 已定 | **BLE-NUS 默认优先** + USB CDC + WiFi WS（`EspTransport`） |
| D7 | 语音 | ✅ 已定 | Opus → **sherpa-onnx 流式 STT** → 实时文字；无波形、不过 webview |
| D8 | 进程模型 | ✅ 已定 | 托盘常驻 + 单实例锁 |
| D9 | master 形态 | ✅ 已定 | 终端标签位于主区 tab 栏；概览面板显示 master 汇报卡片；左侧栏 `[👁]` 可过滤仅看 worker |
| D10 | 智能返回阈值 | 🔜 待调 | 默认 600/3000，上线后按实际数据调优 |

---

## 附录 B：复查结论归档（H1–H6 与修订演进）

### C.1 复查补洞

| # | 洞 | 修法 |
| --- | --- | --- |
| H1 | 音频链路放错边（webview 播放/录音三平台脆） | 全部音频挪到 Rust（cpal/rodio + opus），webview 只收文字 |
| H2 | master 语音 STT 未定义 | sherpa-onnx 本地流式 STT（隐私友好、离线） |
| H3 | 蓝牙完全没设计 | `EspTransport` 三实现抽象，BLE 默认优先 |
| H4 | 关窗即退出 | 托盘常驻，worker 继续跑 |
| H5 | agent 改动审查 UX 缺失 | 右栏 `⚒ Git` 源码控制面板（stage/commit/分支/pull/push） |
| H6 | CLI 版本漂移 | 适配器对 `--help` 容错解析；PTY 生命周期清理防僵尸 |

### C.2 小洞与已知行为

- IPC 载荷上限 → 日志/音频按帧分块
- sqlite 记 agent 运行日志 replay（诊断用）
- 多 agent 并发改同一文件（无 worktree）→ per-agent 子目录 + `⚒ Git` 面板 + git 兜底，文档写明

### C.3 用户四条注意落实

| 注意 | 落实 |
| --- | --- |
| 1. ESP 蓝牙连接，可能主要方式 | BLE-NUS 默认优先（§8.1），P0.5 spike 前置验证 |
| 2. 对话/终端可切换不同 agent | runtime 选择器 + tab 切换（§4.8） |
| 3. 文件路径可拖进对话/终端 | 双通道拖拽（§4.2/§7.3） |
| 4. 编辑器不一定要 Monaco | CodeMirror 6 默认（§7.1） |
