# 快捷键速查表

> **日期:** 2026-08-11
> **范围:** CaPilot IDE 内置终端（bash）以及当前支持的全部 AI runtime —— Claude Code、Codex、OpenCode —— 的键盘快捷键。
> **事实核对:** 各 runtime 的 TUI 快捷键以官方文档为准（下方每节均附来源链接）；CaPilot 侧的拦截/注入行为以代码为准（`ui/components/terminal/XTermPanel.tsx`、`ui/components/layout/Composer.tsx`）。

---

## 1. CaPilot IDE 终端层（所有会话共用）

这部分快捷键由 xterm.js / 应用窗口拦截，**在任何 runtime 的会话里都生效**，不会透传给 PTY。

### 1.1 终端面板（`XTermPanel.tsx`）

| 快捷键 | 功能 | 说明 |
| --- | --- | --- |
| `Ctrl+Shift+C` | 复制选中的终端文本 | xterm 拦截；普通 `Ctrl+C`（无 Shift）仍透传为 SIGINT 给 PTY |
| `Ctrl+F` | 打开终端内搜索条 | 被 xterm 吞掉，不发给 PTY（否则多数 shell 视为 forward-char）；窗口级监听在焦点不在终端时也能触发 |
| `F3` | 下一个匹配 | 搜索条打开时有效 |
| `Shift+F3` | 上一个匹配 | 搜索条打开时有效 |
| `Enter`（搜索条内） | 下一个匹配 | 与 F3 等价 |
| `Shift+Enter`（搜索条内） | 上一个匹配 | 与 Shift+F3 等价 |
| `Esc`（搜索条内） | 关闭搜索 | 清除高亮并还焦给终端 |
| `F1` | 输入框 ⇄ 终端 切换焦点 | 被终端吞掉，不会把 F1 转义序列发给 PTY（多数 CLI 会把它当帮助键） |
| 拖入文件到终端 | 粘贴 shell 转义后的文件路径到 PTY | 单引号包裹，空格/引号安全；带前导空格防粘连 |
| 拖入 commit hash | 把 commit hash 粘贴到 PTY | 来自 Commit Graph 的拖拽 |

### 1.2 Composer 输入层（`Composer.tsx`）

| 快捷键 | 功能 | 说明 |
| --- | --- | --- |
| `Enter` | 发送消息 | 无 Shift 时发送；Codex 场景下文本与 CR 分两次写入 PTY 防粘连 |
| `Shift+Enter` | 换行（不发送） | |
| `Shift+Tab` | 切权限模式 / OpenCode 切主 agent | 见各 runtime 小节；composer 有焦点时生效 |
| `Esc` | 中断当前操作 | 向目标 agent 的 PTY 发原始 ESC 字节（等价于在终端里按 Esc）；弹窗打开时只负责关弹窗 |
| `↑` / `↓`（空输入时） | 浏览已发送草稿历史 | |
| `@` | 文件路径自动补全菜单 | `@` 触发；`↑/↓` 选择，`Enter/Tab` 插入，`Esc` 关闭 |
| `/` | runtime 感知的命令/技能菜单 | `↑/↓` 选择，`Enter/Tab` 选定，`Esc/←` 逐级返回 |
| `!` 前缀 | 终端直发 | 绕过 agent 会话，把 `!` 后的命令原样发进 PTY |
| `F1` | 焦点切换 | 同 1.1，composer 始终持有该窗口级监听 |

### 1.3 文件系统（右键文件面板）

| 快捷键 | 功能 |
| --- | --- |
| `Del` | 删除选中文件/目录 |
| `F2` | 重命名选中文件/目录 |
| `Ctrl/Cmd+C` | 复制选中路径 |
| `Ctrl/Cmd+X` | 剪切选中路径 |
| `Ctrl/Cmd+V` | 粘贴 |

---

## 2. Bash 终端快捷键

> IDE 的 bash 会话通过 PTY 运行真实 bash，下列为 **GNU Readline 默认绑定（emacs 模式）** + bash 交互快捷键。来源：[GNU Readline 手册](https://www.gnu.org/software/bash/manual/html_node/Commands-For-History.html)。

### 2.1 基本操作

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+C` | 中断当前命令 / SIGINT |
| `Ctrl+D` | EOF / 退出登录 shell（空行时） |
| `Ctrl+Z` | 挂起当前进程（可用 `fg` 恢复） |
| `Ctrl+L` | 清屏（等价 `clear`） |
| `Tab` | 命令 / 文件 / 路径补全 |
| `Ctrl+R` | 反向历史搜索（再按 `Ctrl+R` 看更早的匹配） |
| `Ctrl+S` | 正向历史搜索（`stty -ixon` 后有效） |
| `↑` / `↓` | 上一条 / 下一条历史命令 |

### 2.2 行编辑（Readline）

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | 光标移到行首 / 行尾 |
| `Ctrl+B` / `Ctrl+F` | 光标左移 / 右移一个字符 |
| `Alt+B` / `Alt+F` | 光标左移 / 右移一个单词 |
| `Ctrl+K` | 删除光标到行尾（kill） |
| `Ctrl+U` | 删除光标到行首 |
| `Ctrl+W` | 删除光标前一个单词 |
| `Alt+D` | 删除光标后一个单词 |
| `Ctrl+Y` | 粘贴刚被 kill 的文本（yank） |
| `Alt+Backspace` | 删除光标前一个单词 |
| `Ctrl+T` / `Alt+T` | 交换光标前后字符 / 单词 |
| `Alt+U` / `Alt+L` / `Alt+C` | 光标后单词转大写 / 小写 / 首字母大写 |
| `Ctrl+X Ctrl+E` | 用 `$EDITOR` 编辑当前命令行 |
| `Alt+.` | 粘贴上一条命令的最后一个参数 |

> 注意：其中 `Ctrl+F` 在 CaPilot 里被终端拦截为「打开搜索」（见 §1.1），bash 里不会收到 `^F`。

---

## 3. Claude Code（runtime `claude`）

> 官方来源：[Claude Code Keybindings](https://code.claude.com/docs/en/keybindings)。默认绑定大多可在 `~/.claude/keybindings.json` 重绑（`/keybindings` 打开）。

### 3.1 全局

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+C` | 中断当前操作（保留键，不可重绑） |
| `Ctrl+D` | 退出 Claude Code（800ms 内按两次确认） |
| `Ctrl+T` | 显示/隐藏 to-do 清单 |
| `Ctrl+O` | 显示/隐藏 verbose transcript |
| `Ctrl+R` | 打开历史搜索 |
| `↑` / `↓` | 上一条 / 下一条历史 |

### 3.2 Chat 输入区

| 快捷键 | 功能 |
| --- | --- |
| `Enter` | 提交消息 |
| `Esc` | 取消当前输入 |
| `Ctrl+J` | 插入换行（不提交） |
| `Ctrl+L` | 整屏重绘（保留输入）；fullscreen 下 2 秒内按两次 = `/clear` |
| `Cmd/Ctrl+K` | fullscreen 下清屏（2 秒内两次 = `/clear`） |
| `Shift+Tab` | **切换权限模式**（manual → acceptEdits → plan → bypass → auto） |
| `Meta+P` | 打开模型选择器 |
| `Meta+O` | 切换 fast mode |
| `Meta+T` | 切换 extended thinking |
| `Ctrl+_` / `Ctrl+Shift+-` | 撤销上一步 |
| `Ctrl+G` / `Ctrl+X Ctrl+E` | 用外部编辑器编辑 |
| `Ctrl+S` | 暂存当前 prompt |
| `Ctrl+V` | 从剪贴板粘贴图片 |
| `Ctrl+X Ctrl+K` | 停止所有后台 subagent |

### 3.3 补全 / 确认弹窗 / 其它

| 快捷键 | 功能 |
| --- | --- |
| `Tab` / `Esc` / `↑` / `↓` | 接受 / 关闭 / 上 / 下（自动补全菜单） |
| `Y` / `Enter` | 确认（权限确认弹窗） |
| `N` / `Esc` | 拒绝（权限确认弹窗） |
| `Space` | 切换选择 |
| `Shift+Tab` | 权限确认弹窗中循环权限模式 |
| `Ctrl+E` | 切换命令的解释说明 |
| `q` / `Ctrl+C` / `Esc` | 退出 transcript 视图 |

### 3.4 CaPilot 注入的按键

| 按键 | 用途 | 对应代码 |
| --- | --- | --- |
| `Shift+Tab`（`ESC[Z`） | 切权限模式，循环 `manual→acceptEdits→plan→bypass→auto` | `Composer.tsx` `CLAUDE_PERMISSION_CYCLE` |
| `/model <id>` + `Enter` | 换模型 | `Composer.tsx` `applyModel` |
| `/effort low\|medium\|high` + `Enter` | 换思考强度 | `Composer.tsx` `applyThinkingSpeed` |

---

## 4. Codex（runtime `codex`）

> 官方来源：[Developer commands · Interactive shortcuts](https://learn.chatgpt.com/docs/developer-commands?surface=cli)、[CLI customization](https://learn.chatgpt.com/docs/cli-customization)。可用 `/keymap` 交互式重绑，绑定写在 `~/.codex/config.toml` 的 `tui.keymap`。

### 4.1 官方 Interactive shortcuts

| 快捷键 | 功能 |
| --- | --- |
| `@` | 搜索工作区文件并加入 prompt |
| `↑` / `↓` | 恢复草稿历史 |
| `Ctrl+R` | 搜索 prompt 历史（Enter 使用 / Esc 取消） |
| `Ctrl+O`（或 `/copy`） | 复制最新一条已完成的输出 |
| `!` | 行首 `!` 前缀运行本地 shell 命令（走当前审批/沙箱设置） |
| `Tab` | Codex 工作中 → 排队一个 follow-up prompt 给下一轮 |
| `Enter` | Codex 工作中 → 向当前轮注入新指令 |
| `Esc` `Esc` | 空输入框时双击 Esc → 编辑上一条消息并从此处 fork |
| `Ctrl+C`（或 `/exit`） | 关闭会话 |
| `Ctrl+L`（或 `/clear`） | 清空终端视图、保留当前对话（任务执行中禁用） |
| `Alt+R`（或 `/raw`） | 切换 raw scrollback 模式（便于选中/复制） |

### 4.2 编辑 / 推理（官方 + 社区整理的默认绑定）

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+G` | 用 `VISUAL`/`EDITOR` 打开外部编辑器编辑当前 prompt |
| `Shift+↑` / `Shift+↓` | 提高 / 降低 reasoning effort |
| `Alt+.` / `Alt+,` | 提高 / 降低 reasoning effort（备选绑定） |
| `Alt+↑`（或 `Shift+←`） | 编辑最近一条排队的消息 |
| `Ctrl+A` / `Ctrl+E` | 行首 / 行尾 |
| `Ctrl+U` / `Ctrl+K` | 删除到行首 / 行尾 |
| `Ctrl+Y` | 粘贴刚删除的内容 |
| `?` | 打开快捷键总览（cheat sheet，如有） |
| `/keymap` | 查看 / 重绑 TUI 快捷键 |

### 4.3 CaPilot 注入的按键

| 按键 | 用途 | 对应代码 |
| --- | --- | --- |
| `/permissions` 选择器 | 切权限（Read Only → workspace → Full Access），方向键 + `Enter` 驱动；yolo 追加一次确认 | `Composer.tsx` `applyPermissionMode` |
| `/model` 选择器 | 换模型，方向键 + `Enter`；多档模型再弹 reasoning 档位选择 | `Composer.tsx` `applyModel` |
| `Shift+↑`（`ESC[1;2A`）/ `Shift+↓`（`ESC[1;2B`） | 调整 reasoning effort | `Composer.tsx` `applyThinkingSpeed` |

---

## 5. OpenCode（runtime `opencode`）

> 官方来源：[OpenCode Keybinds](https://opencode.ai/docs/keybinds/)（`tui.json`）。默认 leader 键为 `Ctrl+X`：按 `Ctrl+X` 后立即按下组合中的下一个键。
> **CaPilot 私有约定：** 启动时经 `OPENCODE_TUI_CONFIG` 注入会话级 `tui.json`，把 `command_list` 从默认 `Ctrl+P` 重绑为 **`F12`**（`opencode.rs:19-57`）。

### 5.1 会话 / 模型 / agent

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+X` `n` | 新建会话 |
| `Ctrl+X` `l` | 会话列表 |
| `Ctrl+X` `g` | 会话时间线 |
| `Ctrl+R` | 重命名会话 |
| `Ctrl+D` | 删除会话 |
| `Ctrl+C` / `Ctrl+D` / `Ctrl+X` `q` | 退出 |
| `Esc` | 中断当前会话 |
| `Ctrl+X` `c` | 压缩会话 |
| `Tab` | **切换主 agent**（Build ⇄ Plan） |
| `Shift+Tab` | 反向切换主 agent |
| `Ctrl+X` `a` | agent 列表 |
| `Ctrl+X` `m` | 模型列表 |
| `Ctrl+A` | 模型供应商列表 |
| `F2` / `Shift+F2` | 循环最近模型 / 反向循环 |
| `Ctrl+F` | 收藏/取消收藏当前模型 |
| `Ctrl+T` | 循环变体（variant） |

### 5.2 输入编辑（Readline 风格）

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | 行首 / 行尾 |
| `Ctrl+B` / `Ctrl+F` | 左移 / 右移一字符 |
| `Alt+B` / `Alt+F` | 左移 / 右移一单词 |
| `Ctrl+U` | 删除到行首 |
| `Ctrl+K` | 删除到行尾 |
| `Ctrl+W` | 删除前一个单词 |
| `Alt+D` | 删除后一个单词 |
| `Ctrl+D` / `Del` | 删除光标处字符 |
| `Ctrl+Shift+D` | 删除整行 |
| `Ctrl+-` / `Ctrl+.` | 撤销 / 重做 |
| `Ctrl+Z` | 挂起终端（POSIX） |
| `Shift+Enter` / `Ctrl+Enter` / `Alt+Enter` / `Ctrl+J` | 插入换行（不提交） |
| `Ctrl+V` | 粘贴 |

### 5.3 命令面板 / 消息导航 / 弹窗

| 快捷键 | 功能 |
| --- | --- |
| `Ctrl+P` | 命令面板（默认；**CaPilot 中为新会话重绑为 `F12`**） |
| `F12` | 命令面板（CaPilot 注入的会话内绑定） |
| `PageUp` / `PageDown` | 上翻 / 下翻消息（也可用 `Ctrl+Alt+B` / `Ctrl+Alt+F`） |
| `Ctrl+X` `y` | 复制消息 |
| `Ctrl+X` `u` / `Ctrl+X` `r` | 撤销 / 重做消息 |
| `Ctrl+X` `h` | 切换 tips |
| `?`（`help_show`） | 帮助（官方默认 `none`，未绑定） |
| `Ctrl+Alt+K` | which-key 总览 |

### 5.4 弹窗/对话框选择

| 快捷键 | 功能 |
| --- | --- |
| `↑` / `Ctrl+P` | 上一个选项 |
| `↓` / `Ctrl+N` | 下一个选项 |
| `Enter` | 确认 |
| `Esc` | 关闭 |
| `Tab` | 自动补全完成 |
| `Space` | 切换选中（插件/插件列表） |
| `Ctrl+F` | 权限提示全屏模式（`permission.prompt.fullscreen`） |

### 5.5 CaPilot 注入的按键

| 按键 | 用途 | 对应代码 |
| --- | --- | --- |
| `Ctrl+P` + `F12` | 打开命令面板（旧会话靠 Ctrl+P，新会话靠 F12；两个都发无害） | `Composer.tsx` `applyPermissionMode` / `applyModel` |
| 面板中输入命令文本 | 切换 auto-approve（`Enable/Disable auto-approve permissions`）、选模型（输入 `model` 后 Enter） | `Composer.tsx` |
| `Tab` | 切换 Build ⇄ Plan 主 agent | `Composer.tsx` `cycleOpenCodeAgent` |

---

## 6. 附：快捷键冲突注意事项

| 冲突键 | 说明 |
| --- | --- |
| `Ctrl+B` | tmux prefix；`Ctrl+A` 是 GNU screen prefix |
| `Ctrl+Z` | Unix 进程挂起（SIGTSTP） |
| `Ctrl+F` | CaPilot 终端拦截为搜索（§1.1），bash/Claude 中语义不同 |
| `Ctrl+C` / `Ctrl+D` / `Ctrl+M` | Claude Code 保留键，不可重绑（Ctrl+M 与 Enter 在终端等价） |
| `Shift+Enter` | 部分终端无法区分 Shift+Enter 与 Enter，Codex 官方建议改用 `Ctrl+J` |
| `F1` | CaPilot 保留作焦点切换，不会透传给任何 CLI |

---

## 7. 参考链接

- Claude Code Keybindings: https://code.claude.com/docs/en/keybindings
- Codex Developer commands（Interactive shortcuts）: https://learn.chatgpt.com/docs/developer-commands?surface=cli
- Codex CLI customization: https://learn.chatgpt.com/docs/cli-customization
- OpenCode Keybinds: https://opencode.ai/docs/keybinds/
- GNU Readline 手册: https://www.gnu.org/software/bash/manual/html_node/Commands-For-History.html
- CaPilot 实现: `ui/components/terminal/XTermPanel.tsx`、`ui/components/layout/Composer.tsx`、`src-tauri/src/agent_runtime/runtimes/{claude,codex,opencode}.rs`
