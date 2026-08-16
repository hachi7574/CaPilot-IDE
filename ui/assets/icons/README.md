# CaPilot 图标集

一套基于 **Lucide** 和 **Simple Icons** 的单色图标，放在 `ui/assets/icons/`（React 组件 `ui/components/Icon.tsx` 构建时直接加载本目录）。

- **普通图标**：Lucide 风格，`24×24`，`stroke="currentColor"`，`fill="none"`，圆头笔触。
- **品牌 Logo**：Simple Icons 单色版，`fill="currentColor"`，不用彩色原版。
- **颜色全部跟随主题**：SVG 内部用 `style="stroke:var(--icon-color,currentColor)"`（品牌为 `fill`）。
  只要在 CSS 里定义 `--icon-color`，整套图标就会变色；不定义时回退到 `currentColor`（跟随所在元素的 `color`）。

## 用法

### 1. 直接用 SVG 文件（内联 / React）

文件里已经是完整的 `<svg>`，可以直接内联到 JSX 使用：

```tsx
import settingsIcon from "../assets/icons/settings.svg?inline"; // 或直接内联

<span
  className="capilot-icon"
  style={{ ["--icon-color" as any]: "var(--brand)" }} // 只改这一个图标
>
  {/* <svg>…} 内容 */}
</span>
```

如果希望整套图标统一变色，在主题 CSS 里定义一次即可：

```css
:root {
  --icon-color: var(--ink); /* 跟随主题文字色 */
}
```

SVG 里的 `style="stroke:var(--icon-color,currentColor)"` 会优先用 `--icon-color`；没定义时用
`currentColor`，所以也能通过父级 `color` 控制。

### 2. 用 lucide-react（可选）

本项目目前**没有**安装 `lucide-react`。若以后要用，`package.json` 加一行：

```bash
pnpm add lucide-react
```

然后把图标名（见下表「lucide 图标名」）转成 PascalCase 组件导入即可，例如：

```tsx
import { Settings, Search, GitBranch } from "lucide-react";
// 颜色跟随 CSS：.icon { color: var(--icon-color); }
```

> 注意：lucide-react 的组件名 = 文件名 slug 的 PascalCase（`triangle-alert` → `TriangleAlert`，
> `circle-check` → `CircleCheck`，`git-branch` → `GitBranch`，`square-terminal` → `SquareTerminal`）。
> 不同版本图标名可能微调，以 `lucide-react` 实际导出的为准。

---

## 文件清单与 emoji → 图标映射

下表覆盖了当前 UI 里**所有**使用 emoji 的地方（2026-08 全量扫描）。

### 通用 / 系统

| UI 用法（原 emoji） | 图标文件 | Lucide 图标名 | lucide-react 组件 |
|---|---|---|---|
| ⚙ 设置 | `settings.svg` | settings | `Settings` |
| 🔍 搜索 | `search.svg` | search | `Search` |
| 👤 用户 | `user.svg` | user | `User` |
| ☰ 菜单 | `menu.svg` | menu | `Menu` |
| ✕ 关闭 | `close.svg`（别名）· `x.svg` | x | `X` |
| ✓ 勾选 | `check.svg` | check | `Check` |
| ✅ 成功 | `success.svg`（别名）· `circle-check.svg` | circle-check | `CircleCheck` |
| ⚠ 警告 | `warning.svg`（别名）· `triangle-alert.svg` | triangle-alert | `TriangleAlert` |
| ⛔ 禁止 | `ban.svg` · `circle-slash.svg` | circle-slash | `CircleSlash` |
| ℹ 信息 | `info.svg` | info | `Info` |
| 🚀 发布/运行 | `rocket.svg` | rocket | `Rocket` |
| ⚡ 快速/加速 | `zap.svg` | zap | `Zap` |
| ⟳ 加载中 | `loader-circle.svg` | loader-circle | `LoaderCircle` |
| 🔄 刷新 | `refresh-cw.svg` · `rotate-cw.svg` | refresh-cw / rotate-cw | `RefreshCw` / `RotateCw` |
| ⏎ 重做/重复 | `repeat.svg` | repeat | `Repeat` |
| 🔗 链接 | `link.svg` | link | `Link` |
| 🛡 安全 | `shield.svg` | shield | `Shield` |
| ✅ 安全校验 | `shield-check.svg` | shield-check | `ShieldCheck` |
| 🔒 锁定 | `lock.svg` | lock | `Lock` |
| 🔑 密钥 | `key-round.svg` | key-round | `KeyRound` |
| ⌨ 快捷键 | `keyboard.svg` | keyboard | `Keyboard` |
| ⌘ 命令键 | `command.svg` | command | `Command` |
| 🎨 主题/画笔 | `paintbrush.svg` | paintbrush | `Paintbrush` |
| 📋 列表 | `list.svg` | list | `List` |
| ⋯ 更多 | `ellipsis.svg` | ellipsis | `Ellipsis` |
| 👁 显示 | `eye.svg` | eye | `Eye` |
| 🚫 隐藏 | `eye-off.svg` | eye-off | `EyeOff` |
| ↗ 外链 | `external-link.svg` | external-link | `ExternalLink` |

### 文件 / 文件夹

| UI 用法（原 emoji） | 图标文件 | Lucide 图标名 | lucide-react 组件 |
|---|---|---|---|
| 📁 文件夹 | `folder.svg` | folder | `Folder` |
| 📂 打开的文件夹 | `folder-open.svg` | folder-open | `FolderOpen` |
| ＋📁 新建文件夹 | `folder-plus.svg` | folder-plus | `FolderPlus` |
| 📄 文件 | `file.svg` | file | `File` |
| 📝 文本文件 | `file-text.svg` | file-text | `FileText` |
| ＋📄 新建文件 | `file-plus.svg` | file-plus | `FilePlus` |
| 📋 复制/剪贴板 | `copy.svg` · `clipboard.svg` | copy / clipboard | `Copy` / `Clipboard` |
| 🗑 删除 | `trash-2.svg` | trash-2 | `Trash2` |
| ✏ 重命名 | `pencil.svg` | pencil | `Pencil` |
| ▶ 运行文件 | `play.svg` | play | `Play` |
| ＋ 新建 | `plus.svg` | plus | `Plus` |

### 终端 / 会话 / 状态

| UI 用法（原 emoji） | 图标文件 | Lucide 图标名 | lucide-react 组件 |
|---|---|---|---|
| ⌨ 终端 | `terminal.svg` | terminal | `Terminal` |
| ▣ 终端（方形） | `square-terminal.svg` | square-terminal | `SquareTerminal` |
| 🐚 Shell | `shell.svg` | shell | `Shell` |
| 🤖 AI / 机器人 | `bot.svg` | bot | `Bot` |
| 🖥 显示器 | `monitor.svg` | monitor | `Monitor` |
| 💻 笔记本 | `laptop.svg` | laptop | `Laptop` |
| 🔋 电池 | `battery-full.svg` | battery-full | `BatteryFull` |
| 🔌 电源/外接 | `plug.svg` | plug | `Plug` |
| ⏻ 电源键 | `power.svg` | power | `Power` |
| 🔗 USB | `usb.svg` | usb | `Usb` |
| 📶 Wi-Fi/信号 | `wifi.svg` | wifi | `Wifi` |
| 📡 天线/信号 | `radio.svg` | radio | `Radio` |
| 🕸 网络 | `network.svg` | network | `Network` |
| 🌐 网页/全局 | `globe.svg` | globe | `Globe` |
| 🔵🟢● 状态点 | `circle.svg` | circle | `Circle` |
| ◉ 状态（带点） | `circle-dot.svg` | circle-dot | `CircleDot` |
| · 分隔小点 | `dot.svg` | dot | `Dot` |
| 💤 休眠/待机 | `moon.svg` · `moon-star.svg` | moon / moon-star | `Moon` / `MoonStar` |
| ⏱ 计时/耗时 | `timer.svg` | timer | `Timer` |
| 📊 柱状图 | `chart-column.svg` | chart-column | `ChartColumn` |
| 📈 折线图 | `chart-line.svg` | chart-line | `ChartLine` |
| 📅 日期 | `calendar.svg` | calendar | `Calendar` |
| 🗓 日期范围 | `calendar-days.svg` | calendar-days | `CalendarDays` |
| 💾 存储 | `database.svg` | database | `Database` |
| ⚙ CPU | `cpu.svg` | cpu | `Cpu` |
| ≋ 活动 | `activity.svg` | activity | `Activity` |

### Git

| UI 用法（原 emoji） | 图标文件 | Lucide 图标名 | lucide-react 组件 |
|---|---|---|---|
| git（通用） | `git.svg`（Simple Icons 品牌，fill） | —（Lucide 无通用 git） | — |
| ⎇ 分支 | `branch.svg`（别名）· `git-branch.svg` | git-branch | `GitBranch` |
| ● 提交 | `commit.svg`（别名）· `git-commit.svg` | git-commit | `GitCommit` |
| ⑂ 分叉 | `git-fork.svg` | git-fork | `GitFork` |
| ⤴ 合并 | `git-merge.svg` | git-merge | `GitMerge` |
| ⤵ 拉取请求 | `git-pull-request.svg` | git-pull-request | `GitPullRequest` |
| 🕘 历史 | `history.svg` | history | `History` |
| ⬆ 推送 | `upload.svg` | upload | `Upload` |
| ⬇ 拉取 | `download.svg` | download | `Download` |

### 布局 / 方向

| UI 用法（原字符） | 图标文件 | Lucide 图标名 | lucide-react 组件 |
|---|---|---|---|
| → 右 | `arrow-right.svg` | arrow-right | `ArrowRight` |
| ← 左 | `arrow-left.svg` | arrow-left | `ArrowLeft` |
| ↑ 上 | `arrow-up.svg` | arrow-up | `ArrowUp` |
| ↓ 下 | `arrow-down.svg` | arrow-down | `ArrowDown` |
| ↗ 右上 | `arrow-up-right.svg` | arrow-up-right | `ArrowUpRight` |
| ⇄ 交换 | `arrow-left-right.svg` | arrow-left-right | `ArrowLeftRight` |
| ↔ 横向移动 | `move-horizontal.svg` | move-horizontal | `MoveHorizontal` |
| ⌄ 展开 | `chevron-down.svg` | chevron-down | `ChevronDown` |
| › 收起/右 | `chevron-right.svg` | chevron-right | `ChevronRight` |
| ‹ 左 | `chevron-left.svg` | chevron-left | `ChevronLeft` |
| ⌃ 上 | `chevron-up.svg` | chevron-up | `ChevronUp` |
| ▤ 侧栏（右） | `panel-right.svg` | panel-right | `PanelRight` |
| ▥ 侧栏（左） | `panel-left.svg` | panel-left | `PanelLeft` |
| ⬒ 分屏 | `split-square-vertical.svg` | split-square-vertical | `SplitSquareVertical` |
| 三栏 | `columns-3.svg` | columns-3 | `Columns3` |
| 💬 消息 | `message-square.svg` | message-square | `MessageSquare` |
| 📤 发送 | `send.svg` | send | `Send` |
| 🔨 构建/锤子 | `hammer.svg` | hammer | `Hammer` |
| ⚒ 工具 | `wrench.svg` | wrench | `Wrench` |

---

## 品牌 Logo（Simple Icons 单色版）

放在 `brands/` 子目录，全部为 `fill="currentColor"` 单色，同样由 `--icon-color` 控制。

| 品牌 | 文件 | 对应 runtime / 用途 |
|---|---|---|
| Claude | `brands/claude.svg` | `claude` runtime |
| Anthropic | `brands/anthropic.svg` | Anthropic 母品牌 |
| OpenAI | `brands/openai.svg` | `codex` runtime（Codex 属 OpenAI） |
| opencode | `brands/opencode.svg` | `opencode` runtime |
| GitHub | `brands/github.svg` | Git 面板 / 克隆仓库 |
| GNU Bash | `brands/gnubash.svg` | `bash` / `bash-rc` runtime |
| Git | `git.svg`（根目录） | git 通用图标 |

> 品牌图标是 Simple Icons 官方单色 path（CC0），非彩色原版，符合授权与主题要求。
> Simple Icons 没有独立的 "Codex" 或 "Bash" 条目，因此 codex 用 OpenAI、bash 用 GNU Bash 代替。

---

## 维护说明

- 来源：Lucide `lucide-static`（ISC）与 Simple Icons `simple-icons`（CC0 1.0），下载后保留了官方 path 数据。
- 图标命名即 Lucide 官方 slug；`warning / success / close / branch / commit / git.svg` 是按本项目的
  语义提供的别名或品牌补充。
- 所有 SVG 均为 `viewBox="0 0 24 24"`，`width/height=24`，可直接内联或缩放。
