# AI Runtime Integration References

> **日期:** 2026-08-10
> **定位:** CaPilot 对接各 AI 编码 CLI（claude / codex / opencode）的**权威链接 + 已落库集成事实**。
> 原则：仓库里只存「稳定的、属于本项目的事实」；动态的官方细节留在线，用时以官方文档为准。文档站在迭代，以下事实若与官方冲突，**以官方为准**并更新本文件。

---

## 1. 官方文档站点（canonical，始终在线）

| 运行时 | 官方文档 | 项目相关子页 |
| --- | --- | --- |
| **Claude Code** | https://code.claude.com/docs （索引 `llms.txt`） | [CLI reference](https://code.claude.com/docs/en/cli-reference) · [Settings](https://code.claude.com/docs/en/settings) · [Permissions](https://code.claude.com/docs/en/permissions) · [Permission modes（Shift+Tab 环）](https://code.claude.com/docs/en/permission-modes) · [Slash commands（`/model` `/effort`）](https://code.claude.com/docs/en/commands) · [Interactive mode](https://code.claude.com/docs/en/interactive-mode) · [Keybindings](https://code.claude.com/docs/en/keybindings) · [Sessions & resume](https://code.claude.com/docs/en/sessions) · [Model config](https://code.claude.com/docs/en/model-config) |
| **Codex / ChatGPT** | https://learn.chatgpt.com/docs/codex | [CLI 总览](https://learn.chatgpt.com/docs/codex/cli) · [CLI 命令参考（含 flags）](https://learn.chatgpt.com/docs/codex/developer-commands?surface=cli) · [Slash commands（`/model` `/permissions`）](https://learn.chatgpt.com/docs/codex/reference/slash-commands) · [Config file reference](https://learn.chatgpt.com/docs/codex/config-file/config-reference) · [Settings](https://learn.chatgpt.com/docs/codex/reference/settings) · [Permission modes](https://learn.chatgpt.com/docs/codex/permission-modes) · [Sandboxing](https://learn.chatgpt.com/docs/codex/sandboxing) · [Projects & chats](https://learn.chatgpt.com/docs/codex/projects) · [app-server（模型目录）](https://learn.chatgpt.com/docs/codex/app-server) |
| **OpenCode** | https://opencode.ai/docs | [CLI](https://opencode.ai/docs/cli/) · [TUI](https://opencode.ai/docs/tui/) · [Config](https://opencode.ai/docs/config/) · [Keybinds（`tui.json`）](https://opencode.ai/docs/keybinds/) · [Permissions](https://opencode.ai/docs/permissions/) · [Agents](https://opencode.ai/docs/agents/) · [Models](https://opencode.ai/docs/models/) · [Commands](https://opencode.ai/docs/commands/) |

---

## 2. 各 runtime 集成事实

> 适配器入口：`src-tauri/src/agent_runtime/runtimes/<id>.rs`；Composer 的 TUI 驱动逻辑在 `ui/components/layout/Composer.tsx`；运行时感知的 `/` 命令目录在 `src-tauri/src/slash.rs`。
> 「live」指会话运行中的 TUI 操作（通过 PTY 注入按键/命令）；「launch」指进程启动参数。

### 2.1 Claude Code（runtime `claude`）

**Launch**（`claude.rs:178-240`）：
```text
claude --model <id> --permission-mode <native> --allow-dangerously-skip-permissions [--thinking-effort low|medium|high]
```
- 模型：`claude-sonnet-5`（默认）、`claude-opus-5`、`claude-haiku-4-5` —— **hard-code** 在 `claude.rs:91-116`。
- 权限映射 `ask→manual`、`accept_edits→acceptEdits`、`plan→plan`、`auto→auto`、`yolo→bypassPermissions`（`claude.rs:224-240`）。
- `--allow-dangerously-skip-permissions` **总是带上**：它让 bypass 进入 live Shift+Tab 环，本身不启用 bypass。
- 思考强度：`--thinking-effort low|medium|high`（`claude.rs:215-222`）。

**Live 控制（PTY 注入）**：
- 权限环：`Shift+Tab`（`ESC[Z`），顺序 `manual → acceptEdits → plan → bypassPermissions → auto`（Composer `CLAUDE_PERMISSION_CYCLE`，`Composer.tsx:31-37`、`318-339`）。注意与菜单展示顺序不同，勿用 `list_permission_modes` 推导。
- 换模型：`/model <id>` + `Enter`（`Composer.tsx:407-411`）。
- 思考强度：`/effort low|medium|high` + `Enter`（`Composer.tsx:466-479`）。

**Resume / 会话**：
- `claude --resume <key>`；key = cwd 下最新的 `~/.claude/projects/<project-key>/*.jsonl` 文件名（`claude.rs:52-71`）。
- project-key 编码：cwd 中**每个非 `[a-zA-Z0-9]` 字符 → `-`**（含前导 `/`），如 `/home/x/my.proj` → `-home-x-my-proj`（`claude.rs:18-23`，测试见 `claude.rs:248-272`）。

### 2.2 Codex（runtime `codex`）

**Launch**（`codex.rs:309-364`）：
```text
codex [--model <id>] [--ask-for-approval untrusted|never] [--sandbox read-only|workspace-write] | --yolo
      [-c model_reasoning_effort="low|medium|high|xhigh"] --no-alt-screen
```
- 权限：`ask→--ask-for-approval untrusted --sandbox read-only`；`auto→--ask-for-approval never --sandbox workspace-write`；`yolo→--yolo`（`codex.rs:347-364`）。
- `--no-alt-screen`：让 PTY 滚动条与其他 runtime 一致（`codex.rs:317`）。
- 推理强度：`-c model_reasoning_effort="<effort>"`（`codex.rs:336-345`）。

**Live 控制（PTY 注入）**：
- 权限：`/permissions` 选择器，预设顺序 **Read Only → workspace(Default) → Full Access**，方向键 + `Enter` 驱动（`Composer.tsx:294-317`；`yolo` 会多弹一个确认选「Yes, continue anyway」）。
- 换模型：`/model` 选择器，方向键 + `Enter`（`Composer.tsx:395-406`）。
- 推理强度：`Shift+↑`（`ESC[1;2A`）/ `Shift+↓`（`ESC[1;2B`）（`Composer.tsx:445-465`）。

**Resume / 会话**：
- `codex resume <session-id>`（子命令，`codex.rs:321-327`）。
- 会话在 `$CODEX_HOME/sessions`（默认 `~/.codex/sessions`）下递归 `*.jsonl`；找 10 秒内生成且首行 `session_meta.payload.cwd` 匹配的 `id`（`codex.rs:182-235`）。

**模型目录**：`codex app-server --listen stdio://` JSON-RPC（`initialize` + `model/list`），解析 `result.data[]`（`codex.rs:38-101`）；推理档位来自 catalog 的 `supportedReasoningEfforts` + `defaultReasoningEffort`（`codex.rs:126-156`）。

### 2.3 OpenCode（runtime `opencode`）

**Launch**（`opencode.rs:307-346`）：
```text
opencode [--model <provider/model>] [--auto]
```
- 权限：`auto→--auto`（`opencode.rs:340-346`）；`ask`（Normal）走配置里的 allow/ask/deny 规则。
- **没有 thinking/variant launch flag**——思考档位按模型而异，Composer 不显示思考选项（`opencode.rs:301-305`）。

**Live 控制**：
- 自动批准开关：**F12 命令面板**（CaPilot 用会话级 TUI config 把 `command_list` 从默认 Ctrl+P 重绑到 F12，见下）→ 输入 `Enable auto-approve permissions` / `Disable auto-approve permissions` + `Enter`（`Composer.tsx:340-360`）。
- 主 agent 切换：`Tab` 在 Build ⇄ Plan 之间切（`Composer.tsx:518-536`）。

**TUI config（本项目私有约定）**：启动时写 `$XDG_CACHE_HOME/capilot-ide/opencode-tui/<session-id>.json`，内容
```json
{ "$schema": "https://opencode.ai/tui.json", "keybinds": { "command_list": "f12" } }
```
经 `OPENCODE_TUI_CONFIG` 环境变量传入（`opencode.rs:19-57`）。`tui.json` 的 keybinds 格式见 [opencode.ai/docs/keybinds](https://opencode.ai/docs/keybinds/)（默认 `command_list: "ctrl+p"`，可设 `"none"` 禁用）。

**Resume / 会话**：
- `opencode --session <id>`（`opencode.rs:324-330`）；`opencode session list --format json --max-count 20` 按 cwd + 10 秒窗口取最近 `id`（`opencode.rs:233-264`）。
- 模型选择记录在 `$XDG_STATE_HOME/opencode/model.json`（默认 `~/.local/state/opencode/model.json`），取 `recent[0]` 的 `providerID/modelID`（`opencode.rs:67-86`）。
- 模型目录：`opencode models --verbose`，解析 column-0 `provider/model` 头 + 后续 catalog JSON 块（含 `name`，用于 TUI 对话框按名输入）（`opencode.rs:163-224`）。

---

## 3. 易变事实速查（改代码前先查这里）

这些是**最脆弱的接缝**——runtime 升级改了键位/flag/顺序，CaPilot 就要跟着改：

| # | 事实 | 当前值 | 硬编码位置 | 官方依据 |
| --- | --- | --- | --- | --- |
| 1 | Claude 权限环顺序 | manual → acceptEdits → plan → bypassPermissions → auto | `Composer.tsx` `CLAUDE_PERMISSION_CYCLE` | [permission-modes](https://code.claude.com/docs/en/permission-modes)（Shift+Tab cycle） |
| 2 | Claude native 权限名 | `acceptEdits` / `bypassPermissions`（驼峰） | `claude.rs:224-240` | [cli-reference](https://code.claude.com/docs/en/cli-reference) |
| 3 | Claude 模型列表 | sonnet-5 / opus-5 / haiku-4-5 | `claude.rs:91-116` | [model-config](https://code.claude.com/docs/en/model-config) |
| 4 | Claude 项目目录编码 | 非 alnum → `-`，含前导 `/` | `claude.rs:18-23` | 实测 ~/.claude/projects |
| 5 | Codex 权限选择器顺序 | Read Only → workspace → Full Access | `Composer.tsx:294-317` | [permission-modes](https://learn.chatgpt.com/docs/codex/permission-modes) |
| 6 | Codex launch 权限/沙箱 flag | `--ask-for-approval untrusted\|never`、`--sandbox read-only\|workspace-write`、`--yolo` | `codex.rs:347-364` | [developer-commands](https://learn.chatgpt.com/docs/codex/developer-commands?surface=cli) |
| 7 | Codex 推理档位 | low / medium / high / xhigh（catalog 驱动） | `codex.rs:336-345` + `126-156` | catalog `supportedReasoningEfforts` |
| 8 | OpenCode 命令面板键 | 本项目重绑 `command_list → f12`（默认 ctrl+p） | `opencode.rs:19-57` | [keybinds](https://opencode.ai/docs/keybinds/) |
| 9 | OpenCode `--auto` / 权限 | `--auto` = auto-approve；无其它模式 flag | `opencode.rs:340-346` | [permissions](https://opencode.ai/docs/permissions/) |
| 10 | Codex 会话 `resume` 子命令 | `codex resume <id>`（非 flag） | `codex.rs:321-327` | [developer-commands](https://learn.chatgpt.com/docs/codex/developer-commands?surface=cli) |
| 11 | Claude 会话目录 | `~/.claude/projects/<project-key>/*.jsonl` | `claude.rs:52-71` | [sessions](https://code.claude.com/docs/en/sessions) |

> 编号 1、5、8 都是「顺序/键位」型事实，**最容易随版本漂移**。改它们时，确认当前 TUI 实际行为后再动代码，别只凭旧文档。

---

## 4. 保持新鲜

- 本文件事实核对日期 **2026-08-10**。
- 三个 runtime 官方文档都在周更级迭代。改 adapter 前：
  1. 先在 §1 的对应子页确认最新 flag/键位/顺序；
  2. 更新 §3 表格与对应代码锚点；
  3. 若 `claude`/`codex`/`opencode` CLI 已安装到本机，可 `--help` / 实测 TUI 行为做最终校验（本机 UI 自动化受限，见运行手册）。
