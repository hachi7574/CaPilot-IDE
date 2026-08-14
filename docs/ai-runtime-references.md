# AI Runtime Integration References

> **日期:** 2026-08-15
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
- `claude --resume <key>`；key 是当前 Agent 的 Claude provider session id，由每 Agent 的 `SessionStart` hook 写入侧车并持久化。恢复和 usage 都只读取 `~/.claude/projects/<project-key>/<key>.jsonl`；**禁止**按 cwd 选最新文件，避免同目录的 IDE/standalone 会话串线。
- project-key 编码：cwd 中**每个非 `[a-zA-Z0-9]` 字符 → `-`**（含前导 `/`），如 `/home/x/my.proj` → `-home-x-my-proj`（`claude.rs:18-23`，测试见 `claude.rs:248-272`）。
- cache usage 按 assistant `message.id` 去重后累计；`message.model` 作为实际运行模型只用于 UI 展示，不覆盖配置模型，因此 `/model` 切换和 catalog checkmark 仍按 Agent 配置匹配。命中量为 0 但总输入有效时必须上报 `0`，不能上报 `null`。

**状态上报（lifecycle hooks，v2.1.228 实测）**：
- 每个由 CaPilot 启动的 claude 会话额外注入 `--settings ~/CaPilot/status/hooks.json` + env `CAPILOT_AGENT_ID`/`CAPILOT_STATUS_DIR`。`--settings` 是**附加** settings 源——不动用户全局 `~/.claude/settings.json`，standalone claude 不受影响（`claude.rs:178-230`）。
- hook 脚本 `~/CaPilot/status/hook.sh`（app 自写，POSIX sh）读 stdin 载荷里的 `hook_event_name`，把生命周期事件映射为状态写入 `~/CaPilot/status/<agent_id>.json`：`SessionStart→idle`、`UserPromptSubmit|PostToolUse|PostToolUseFailure|PostToolBatch→working`、`PreToolUse` 视 `tool_name` 而定（claude `AskUserQuestion` / codex `tool/requestUserInput`（item 流 `item/tool/requestUserInput`）→`awaiting_choice`，其余→`working`）、`PermissionRequest` 同样视 `tool_name` 而定（question 工具→`awaiting_choice`，其余→`waiting_input`——**必须**：claude 对 AskUserQuestion 在 PreToolUse 之后还会发一个 PermissionRequest，若不按 tool_name 分流会把 `awaiting_choice` 覆盖成 `waiting_input`）、`Stop|StopFailure→idle`、`SessionEnd→dormant`（`claude.rs:12-42`、`181-210`；`status_hooks.rs`）。
- 已实测（`claude -p --settings`）事件序列：`SessionStart → UserPromptSubmit → PreToolUse → PostToolUse → Stop → SessionEnd` 全部触发。`PermissionRequest` 未在 print mode 触达（需交互权限提示），按官方文档映射。
- 前端 TabBar 每秒轮询 `agent_status_read`（Rust 读侧车文件），`effectiveAgentStatus` 优先采用 hook 状态；`waiting_input`/`awaiting_choice` 在有近期输出时降级为 `运行中`（权限批准/用户作答后长工具流式输出不误报待确认/待选择）。

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

**状态上报（lifecycle hooks，0.147.0 实测）**：
- codex **没有** claude 的 `--settings` 式按会话注入点；hook 只能从 `$CODEX_HOME/hooks.json`（默认 `~/.codex/hooks.json`）或 config 层的 `[[hooks.<Event>]]` TOML 加载。`-c hooks.file=` 会被静默忽略（`HooksToml` 无 `file` 字段，flatten 结构内联事件）。
- CaPilot 用 **per-session config profile**：启动前写 `$CODEX_HOME/capilot-<agent_id>.config.toml`，内容是 `[[hooks.<Event>]]` 数组表、每条 `type="command"` 调共享 `~/CaPilot/status/hook.sh`，然后 launch 加 `-p capilot-<id> --dangerously-bypass-hook-trust`。profile 是叠加层——**不动用户真实 `config.toml` / `hooks.json`**；会话删除时清理（`codex.rs` `write_status_profile` / `remove_status_profile`）。
- `--dangerously-bypass-hook-trust` 必需：profile hooks 不在用户持久化信任里，该 flag 让本次调用跳过信任提示（脚本是 app 自写，安全）。副作用是启动时 stderr 打一行 warning（去掉 flag 会触发 codex 的**交互式 startup hooks review TUI**，在 PTY 内阻塞会话启动，更糟；`trusted_hash` 预信任方案需复刻 codex 内部 hash，跨版本易碎，未采用）。
- profile 里 `SessionEnd` 的 hook 必须写 `timeout = 3`：codex 会把 SessionEnd hook 钳制到 3s（默认 1s、上限 3s）并对大于 3s 的声明在启动时告警；其余事件写 `timeout = 5`（hook.sh 是亚毫秒 sh 脚本，3s 绰绰有余）。（`codex.rs` `write_status_profile`，实测 0.147.0）
- 事件集（codex `HookEventsToml`）：`SessionStart`/`UserPromptSubmit`/`PreToolUse`/`PostToolUse`/`PermissionRequest`/`Stop`/`SessionEnd`（**无** `PostToolUseFailure`/`PostToolBatch`/`StopFailure`）。payload stdin 与 claude 同构（含 `hook_event_name`），hook.sh 零改动复用；hook 进程**不在沙箱内**，可写宿主机侧车 `~/CaPilot/status/<agent_id>.json`（实测 `exec` 全链路：SessionStart→idle、UserPromptSubmit→working、Stop→idle）。

### 2.3 OpenCode（runtime `opencode`）

**Launch**（`opencode.rs:307-346`）：
```text
opencode [--model <provider/model>] [--auto]
```
- 权限：`auto→--auto`（`opencode.rs:340-346`）；`ask`（Normal）走配置里的 allow/ask/deny 规则。
- **没有 thinking/variant launch flag**——思考档位按模型而异（variant 集在 `models --verbose` catalog 的 `variants` 对象里），Composer 不显示固定档位菜单，改用 **Ctrl+T 循环切换**（见下 `variant_cycle`）。

**Live 控制**：
- 自动批准开关：**F12 命令面板**（CaPilot 用会话级 TUI config 把 `command_list` 从默认 Ctrl+P 重绑到 F12，见下）→ 输入 `Enable auto-approve permissions` / `Disable auto-approve permissions` + `Enter`（`Composer.tsx:340-360`）。
- 主 agent 切换：`Tab` 在 Build ⇄ Plan 之间切（`Composer.tsx:518-536`）。
- **思考强度循环：`ctrl+t` = `variant_cycle`**（默认键，`packages/tui/src/config/keybind.ts`）。按下后在当前模型的 variants 间循环：`默认 → variant[0] → … → 末位 → 默认`（`variant.cycle()`，`packages/tui/src/context/local.tsx`）。Composer 对 opencode 目标把 **Ctrl+T 重定向**为给 PTY 发 ``（`Composer.tsx` `cycleOpenCodeVariant`），并在 opencode action 行加了 ⚡ 按钮；按钮标签读 `$XDG_STATE_HOME/opencode/model.json` 的 `variant.<provider/model>`（`opencode.rs` `current_variant` / `lib.rs` `opencode_current_variant`），`default`/缺失显示 `Default`。无 variant 的模型按 Ctrl+T 是无害 no-op。variant 不写 CaPilot 会话配置——它由 opencode 自己按模型全局记录。

**TUI config（本项目私有约定）**：启动时写 `$XDG_CACHE_HOME/capilot-ide/opencode-tui/<session-id>.json`，内容
```json
{ "$schema": "https://opencode.ai/tui.json", "keybinds": { "command_list": "f12" } }
```
经 `OPENCODE_TUI_CONFIG` 环境变量传入（`opencode.rs:19-57`）。`tui.json` 的 keybinds 格式见 [opencode.ai/docs/keybinds](https://opencode.ai/docs/keybinds/)（默认 `command_list: "ctrl+p"`，可设 `"none"` 禁用）。

**TUI 鼠标协议（1.18.18 实测）**：OpenCode 启动时启用 DECSET `1000/1002/1003` 鼠标追踪和 SGR `1006` 编码。CaPilot 为保留 xterm 原生文本选择，会从 PTY 输出中剥离追踪 enable，再自行按指针所在单元格发送 `CSI < 64|65 ; col ; row M` 滚轮报告。这里有三个必须同时保持的回归约束：
- 禁止 xterm 在 alternate buffer 中把滚轮降级成 `ArrowUp/ArrowDown`，否则 OpenCode 会在任何指针位置切换 prompt 历史；
- wheel 监听必须用 DOM capture phase，避免 xterm 内层 viewport 在冒泡阶段先消费事件；
- OpenCode PTY 会常驻并跨前端 xterm 重挂载，且有界 PTY replay 可能已淘汰最初的 `CSI ? 1006 h`。因此 OpenCode 的 SGR 支持是 adapter 固有契约，不能只依赖当前 xterm 是否观察到启动序列，否则长会话重挂载后滚轮和点击会再次失效。

实现位于 `XTermPanel.tsx` + `mouseProtocol.ts`；回归测试为 `pnpm test:terminal-mouse`。

**Resume / 会话**：
- `opencode --session <id>`。新会话由每 Agent 的 OpenCode 插件从事件总线捕获根 session `sessionID` 并写入侧车；usage/resume 只使用持久化 id。旧 Agent 仅在 cwd 一致且 Agent 创建前 2 秒至后 30 秒内**恰好一个**候选时恢复，歧义时不猜测。
- 模型选择记录在 `$XDG_STATE_HOME/opencode/model.json`（默认 `~/.local/state/opencode/model.json`），取 `recent[0]` 的 `providerID/modelID`（`opencode.rs:67-86`）。
- 模型目录：`opencode models --verbose`，解析 column-0 `provider/model` 头 + 后续 catalog JSON 块（含 `name`，用于 TUI 对话框按名输入）（`opencode.rs:163-224`）。

**状态上报（插件事件总线，1.18.16 实测）**：
- opencode **没有** claude 式 `--settings` 或 codex 式 config profile 的 shell hook 面；唯一的 hook 点是进程内 JS 插件事件总线。CaPilot 用**按会话追加的 config dir**：启动前写 `$XDG_CACHE_HOME/capilot-ide/opencode-status/<agent_id>/plugin/capilot-status.js`（app 自写的 ESM 插件），launch env 加 `OPENCODE_CONFIG_DIR=<该目录>` + `CAPILOT_AGENT_ID`/`CAPILOT_STATUS_DIR`（`opencode.rs` `write_status_plugin`/`launch_env`）。
- `OPENCODE_CONFIG_DIR` 是**追加**语义：opencode 把该目录加进 `ConfigPaths.directories()` 搜索链（`packages/opencode/src/config/paths.ts`），用户的全局配置（`~/.opencode/opencode.json[c]` 等）**照常加载**；实测 run log 先列 `~/.opencode/*` 再列 override 目录。插件的 `{plugin,plugins}/*.{ts,js}` 与全局插件一起被扫描（`config/plugin.ts` `ConfigPlugin.load`），故只对本会话生效，独立 `opencode` 运行不受影响。
- 插件监听事件总线（`Hooks.event`）：首次观察到 cwd 一致、无 parent 的根 session 后锁定 `sessionID`，后续 child session 不得覆盖；生命周期映射保持 `session.status` busy/retry→`working`、idle→`idle`，permission/question 事件分别映射待确认/待选择。侧车格式为 `{"status","ts","session_id"}`；插件加载时先写一次无 session id 的 `idle`，根 session 创建后补齐 id。
- 会话删除时清理整个 `opencode-status/<agent_id>/` 目录（`opencode.rs` `remove_status_plugin`；`sessions_delete` 调用）。插件写失败只降级为无 hook（沿用 PTY 活动启发式），不 abort 启动。

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
| 12 | Codex 上下文占用 + 缓存命中数据源 | 通过 Agent 持久化的 `resume_key`（Codex session id）精确定位会话 JSONL，并复核 cwd，避免同目录多 Agent / standalone Codex 串数据。新会话从专属 hook sidecar 的 `session_id` 绑定；旧数据或捕获竞态按 UUIDv7 内嵌时间与 Agent `created_at` 在 5s 窗口内最近匹配，同时强制 cwd 一致，并把恢复出的 key 回写数据库。读取 `token_count` 事件：`payload.info.last_token_usage.total_tokens`（used）+ `payload.info.model_context_window`（max，`task_started` 也有）。**会话累计**命中率分子/分母 = 所有 `token_count` 的 `cached_input_tokens`（旧版命名 `cache_read_input_tokens`，二者都兼容）与 `input_tokens`——codex 的 `input_tokens` **已含缓存部分**（实测 `total_tokens == input_tokens + output_tokens`），所以分母就是 `input_tokens`，不得再加回缓存 | `codex.rs` `context_usage`/`latest_usage_from_content` | codex 会话 JSONL（实测 0.147.0） |
| 13 | OpenCode 上下文占用 + 缓存命中数据源 | 通过 Agent 持久化的 `resume_key` 精确定位 `opencode.db.session.id` 并复核 cwd，禁止按 cwd 取最新。used = 该 session 最新 `step-finish.tokens.total`；max = 实际 assistant model 对应 catalog `limit.context`。**会话累计**命中率直接使用 session 聚合列：分子 `tokens_cache_read`，分母 `tokens_input + tokens_cache_read + tokens_cache_write`（input 不含 cache）。聚合列避免重复 part 二次累计 | `opencode.rs` `context_usage`/`session_cache_stats` | opencode 本地 SQLite + `models --verbose`（实测 1.18.16） |
| 14 | OpenCode catalog 缓存 | 进程内 5min TTL + 落盘 `$XDG_CACHE_HOME/capilot-ide/opencode-model-limits.json`（`provider/model → context`）。进程重启后冷路径先读盘（~0ms），避免首载跑 ~0.7s 的 `models --verbose` 子进程；运行中 TTL 过期仍刷新 CLI 并写回盘 | `opencode.rs` `catalog_limit_context` | 实测：子进程 ~0.66s，DB 查询 ~50ms |
| 15 | Codex hook 注入方式 | **无** claude 式 `--settings`；`-c hooks.file=` 被忽略（`HooksToml` 无该字段）。按会话用 `-p capilot-<id>` config profile（`$CODEX_HOME/capilot-<id>.config.toml` 内联 `[[hooks.<Event>]]`）+ `--dangerously-bypass-hook-trust`。profile 事件集无 `PostToolUseFailure`/`PostToolBatch`/`StopFailure` | `codex.rs` `write_status_profile`/`spawn_interactive` | 实测 0.147.0（payload 与 claude 同构，hook 进程可写宿主机） |
| 16 | OpenCode hook 注入方式 | **无** shell hook 面；唯一入口是 JS 插件事件总线。按会话用 `OPENCODE_CONFIG_DIR` 注入插件；插件除生命周期状态外，锁定首次观察到的 root `sessionID` 并写入 Agent 专属侧车，child session 不覆盖。事件字段兼容 v1 `properties` / v2 `data` | `opencode.rs` `STATUS_PLUGIN`/`write_status_plugin`/`launch_env` | 实测 1.18.16 + 本地 plugin SDK 类型 |
| 17 | **launch override 会丢弃 hook 注入** | Settings → 已安装 → ⚙ 的 args override **整体替换** adapter 参数列表，会把 claude 的 `--settings`、codex 的 `-p` profile 一起丢掉 → hook 永不触发、状态退回 PTY 活动启发式（长工具间隙误报空闲、输入回显误报运行中）。`lib.rs` spawn 在 override 替换 args 后**重追加** `mode_args`+`speed_args`+`status_hook_args` | `lib.rs` spawn（`replaced` 分支）+ adapter `status_hook_args` | 实测 2026-08-13（claude.args=`--model claude-sonnet-5`、codex.args=`--no-alt-screen`） |
| 18 | Claude 上下文占用 + 缓存命中数据源 | 通过 Agent `resume_key` 精确读取 `<project-key>/<session-id>.jsonl`，禁止按 cwd 取最新。跳过 sidechain，并按 assistant `message.id` 去重。used = 最后一个非零 usage 的 input + cache creation + cache read + output；累计命中率分子/分母 = cache read 与 input + cache creation + cache read。`message.model` 只作为实际模型展示 | `claude.rs` `context_usage`/`parse_transcript_usage`/`read_transcript` | 实测 transcript JSONL |
| 19 | OpenCode TUI 鼠标协议 | SGR 1006；滚轮 `64/65` + 1-based cell 坐标。xterm tracking enable 被剥离以保留文本选择，IDE 用 capture-phase wheel 手工转发并禁用 alternate-buffer 方向键降级；resident PTY 重挂载不得依赖启动帧仍在 replay 中 | `XTermPanel.tsx` + `mouseProtocol.ts` | 实测 OpenCode 1.18.18；`pnpm test:terminal-mouse` |
| 20 | OpenCode 思考强度键 | `ctrl+t` = `variant_cycle`（默认键）；循环顺序 = catalog `variants` 对象键序，`默认 → 首 → … → 末 → 默认`。Composer 把 opencode 目标的 Ctrl+T 重定向为给 PTY 发 ``，按钮标签读 `model.json` `variant.<provider/model>` | `Composer.tsx` `cycleOpenCodeVariant` + `opencode.rs` `current_variant` | [keybinds](https://opencode.ai/docs/keybinds/) + 源码 `packages/tui/src/context/local.tsx`（1.18.18 实测） |

> 编号 1、5、8、20 都是「顺序/键位」型事实，**最容易随版本漂移**。改它们时，确认当前 TUI 实际行为后再动代码，别只凭旧文档。

**OpenCode 首轮低命中基线（1.18.18 / Zen DeepSeek V4 Flash Free，2026-08-15 实测）**：两个全新会话的首轮分别为 `input=8886, cache_read=1792`（16.78%）和 `input=8370, cache_read=1792`（17.63%）。固定的 1792 说明首轮大概率只复用了 provider 可识别的公共前缀；OpenCode/Zen 只返回 token 数量，不返回命中的具体文本区间，因此不能进一步断言是哪一段。Build agent 首轮还会带上 system prompt、工具 schema、权限、环境和项目规则；CaPilot-Ide 会额外加载 2,216 字节的 `CLAUDE.md`，两个项目的非缓存 input 相差 516 tokens（路径与环境差异也可能占一部分）。同一台机器上的长工具循环会话累计命中率可达到 92.05%，所以首轮低值不能通过改公式“修高”，应保留 provider 原始会计语义并把它视为冷启动基线。

**缓存命中率 chip（composer target 行）**：`AgentUsage` 新增 `cache_hit_tokens` / `cache_total_input_tokens` 两个**会话累计**字段（wire camelCase，`adapter.rs` `AgentUsage`）。各 adapter 按自己 runtime 的会计语义归一化后再上报（Claude/opencode：分子 `cache_read`、分母 `input+cache_read(+cache_write/creation)`；codex：分子 `cached_input_tokens`、分母 `input_tokens`），前端只算 `cache_hit/cache_total` 比例、不做跨 runtime 换算。只有两者都在且分母 > 0 才渲染 chip（`ui/components/layout/CacheHitRate.tsx`）；bash 等无 `context_usage` 的 runtime 永不渲染。

新增 Provider 必须通过 [context-window-usage.md](context-window-usage.md#mandatory-provider-invariants) 的 identity、隔离、0 值、去重/聚合边界、实际模型和双会话回归清单；只在单会话 happy path 上能显示数字不算完成。

---

## 4. 保持新鲜

- 本文件事实核对日期 **2026-08-15**。
- 三个 runtime 官方文档都在周更级迭代。改 adapter 前：
  1. 先在 §1 的对应子页确认最新 flag/键位/顺序；
  2. 更新 §3 表格与对应代码锚点；
  3. 若 `claude`/`codex`/`opencode` CLI 已安装到本机，可 `--help` / 实测 TUI 行为做最终校验（本机 UI 自动化受限，见运行手册）。
