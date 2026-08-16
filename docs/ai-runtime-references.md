# AI Runtime Integration References

> **日期:** 2026-08-17（增补 ACP 双轨索引；PTY 事实仍以 08-16 核对为准）
> **定位:** CaPilot 对接各 AI 编码 CLI（claude / codex / opencode / dsh / pi）的**权威链接 + 已落库集成事实**；并索引 **ACP** 通用通道。
> 原则：仓库里只存「稳定的、属于本项目的事实」；动态的官方细节留在线，用时以官方文档为准。文档站在迭代，以下事实若与官方冲突，**以官方为准**并更新本文件。

---

## 1. 官方文档站点（canonical，始终在线）

| 运行时 | 官方文档 | 项目相关子页 |
| --- | --- | --- |
| **Claude Code** | https://code.claude.com/docs （索引 `llms.txt`） | [CLI reference](https://code.claude.com/docs/en/cli-reference) · [Settings](https://code.claude.com/docs/en/settings) · [Permissions](https://code.claude.com/docs/en/permissions) · [Permission modes（Shift+Tab 环）](https://code.claude.com/docs/en/permission-modes) · [Slash commands（`/model` `/effort`）](https://code.claude.com/docs/en/commands) · [Interactive mode](https://code.claude.com/docs/en/interactive-mode) · [Keybindings](https://code.claude.com/docs/en/keybindings) · [Sessions & resume](https://code.claude.com/docs/en/sessions) · [Model config](https://code.claude.com/docs/en/model-config) |
| **Codex / ChatGPT** | https://learn.chatgpt.com/docs/codex | [CLI 总览](https://learn.chatgpt.com/docs/codex/cli) · [CLI 命令参考（含 flags）](https://learn.chatgpt.com/docs/codex/developer-commands?surface=cli) · [Slash commands（`/model` `/permissions`）](https://learn.chatgpt.com/docs/codex/reference/slash-commands) · [Config file reference](https://learn.chatgpt.com/docs/codex/config-file/config-reference) · [Settings](https://learn.chatgpt.com/docs/codex/reference/settings) · [Permission modes](https://learn.chatgpt.com/docs/codex/permission-modes) · [Sandboxing](https://learn.chatgpt.com/docs/codex/sandboxing) · [Projects & chats](https://learn.chatgpt.com/docs/codex/projects) · [app-server（模型目录）](https://learn.chatgpt.com/docs/codex/app-server) |
| **OpenCode（PTY）** | https://opencode.ai/docs | [CLI](https://opencode.ai/docs/cli/) · [TUI](https://opencode.ai/docs/tui/) · [Config](https://opencode.ai/docs/config/) · [Keybinds（`tui.json`）](https://opencode.ai/docs/keybinds/) · [Permissions](https://opencode.ai/docs/permissions/) · [Agents](https://opencode.ai/docs/agents/) · [Models](https://opencode.ai/docs/models/) · [Commands](https://opencode.ai/docs/commands/) |
| **OpenCode ACP / ACP 通用** | [Agent Client Protocol](https://github.com/agentclientprotocol/agent-client-protocol) · 本机 `opencode acp` | **方案与验收：** [`acp-runtime-plan.md`](./acp-runtime-plan.md)（§12 / 附录 A）· **状态：** [`acp-dev-status.md`](./acp-dev-status.md) · runtime id **`acp:opencode`**（≠ PTY `opencode`） |
| **dsh（DeepSeek Harness）** | https://github.com/deepseek-ai/dsh（CLI 包 `@deepseek-ai/dsh`，TUI 为 `@deepseek-harness-tui/dsh-tui` 插件，随 `dsh plugin --profile dsh-tui add @deepseek-harness-tui/dsh-tui` 安装） | 已落库事实以本文件 §2.4 / §3 为准；上游为 Commander launcher + Cordis app，官方文档在仓库 README 与 `dsh-tui` 插件 README（本地 `~/.dsh/profiles/dsh-tui` 下有安装副本） |
| **pi（Earendil pi-coding-agent）** | https://www.npmjs.com/package/@earendil-works/pi-coding-agent（CLI `pi`，本机 `~/APP/n/bin/pi`；官方细节在 npm 包 README 与 `dist` 源码注释） | 已落库事实以本文件 §2.5 / §3 为准；`pi --help` / `pi --list-models` / `pi --list-providers` 为本地权威 |

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

### 2.3a ACP 双轨（runtime `acp:<id>`，锚点 `acp:opencode`）

> **完整设计 / §12 验收 / 附录 A：** [`acp-runtime-plan.md`](./acp-runtime-plan.md)  
> **开发相位：** [`acp-dev-status.md`](./acp-dev-status.md)  
> **实现：** `src-tauri/src/agent_runtime/acp/` · UI `AcpSessionPanel` · Settings → ACP

**与 PTY `opencode` 的硬边界：**

| | PTY `opencode` | ACP `acp:opencode` |
| --- | --- | --- |
| Transport | portable-pty + xterm | stdio NDJSON JSON-RPC |
| Launch | `opencode` + `OPENCODE_TUI_CONFIG` / status 插件 | `opencode acp`（descriptor）；**不**注入 TUI config / status 插件 |
| UI | `XTermPanel` + Composer 方言（F12 / Ctrl+T variant…） | `AcpSessionPanel` + `acp_prompt` / `acp_cancel` |
| 写输入 | `agent_write` | **禁止** `agent_write`；走 `session/prompt` |
| resume_key | OpenCode 内部 session 侧车 | ACP `sessionId`（`session/new` 或 `session/load`） |
| 权限 | TUI / `--auto` | Host `request_permission` → 默认 ask（MVP） |

**Descriptor：** `~/CaPilot/acp-agents.json`（用户）∪ 内置默认 `opencode` → `acp:opencode`。Settings CRUD：`acp_list_agents` / `acp_upsert_agent` / `acp_remove_agent`。  
**Usage：** `session/update` → `usage_update{used,size}` → 面板 + `AgentInfo.last_usage`。  
**安全：** `fs/write` 关；`fs/read` cwd 沙箱；见 `security-review.md` §6。

### 2.4 dsh（runtime `dsh`，DeepSeek Harness dsh-tui）

> 适配器 `dsh.rs`；Composer 的 dsh 分支在 `Composer.tsx`；`/` 命令目录在 `slash.rs` `DSH_COMMANDS`。

**Launch**（`dsh.rs:546-602`）：
```text
dsh --profile dsh-tui --patch <每会话临时 patch>
```
- 无 argv 注入模型/权限/effort：模型 + effort 经 `--patch` 每会话 overlay（cordis patch 语义**整体替换** dsh-tui 配置行）；权限经 env `DSH_PERMISSION_MODE=read-only|workspace-write|danger-full-access`（`dsh.rs:316-322`）。
- `NODE_ENV=production` 必须：dsh 的 React dev renderer 会累积 unbounded `performance.measure()` 记录、长会话 OOM（`dsh.rs:580-586`）。
- patch 写在 `$XDG_CACHE_HOME/capilot-ide/dsh/<safe-id>.patch.yml`（`~/.cache` 回退），仅本次 spawn 生效；用户全局 `~/.dsh/profiles/dsh-tui/cordis.yml`、`~/.dsh-tui/model.json` 不动。会话删除时清理（`dsh.rs:324-382`、`remove_session_patch`）。
- patch 内容：`- id: dsh-tui` + `config: {provider: <provider>, model: <id>, [effort: off|high|max], sessionId: !!js process.env.DSH_TUI_RESUME_SESSION ?? process.env.DSH_CC_RESUME_SESSION ?? undefined}`。`sessionId` **必须**无条件重写——dsh-tui 只从该 config 键读 resume（由 `DSH_TUI_RESUME_SESSION` env 喂入，0.7.2 起为 canonical、旧 `DSH_CC_RESUME_SESSION` 作 dual-read 兼容），patch 整体替换配置行会丢 resume 缝（`dsh.rs:347-374`）。
- 模型：**settings.yaml 探测**（provider-qualified，`dsh.rs` `model_catalog_probe`）——`agent-default-model` 标默认，`deepseek-official` 路由内建 flash/pro，另有 `opencode-go/deepseek-v4-flash` 等 pi-ai 提供商标的模型；default = settings.yaml 的 `agent-default-model`（实测 `opencode-go/deepseek-v4-flash`）。
- 权限映射 `ask→read-only`、`auto→workspace-write`、`yolo→danger-full-access`（yolo `requires_confirmation: true`）（`dsh.rs:498-519`）。
- 思考档位：`auto→max`（deepseek-official 默认）、`fast→off`、`mid→high`、`high→max`；**pi-ai 路由（opencode-go 等）固定 `effort: off`**——pi-ai 对未声明 reasoning 元数据的模型只支持 off，钉 off 可覆盖机器残留的 effort.json（`dsh.rs:301-311`、`521-544`）。

**Live 控制（PTY 注入）**：
- 权限 **live 切换**：dsh-base 挂载的 `dsh-permission-presets` 提供 `/permission <preset>`（read-only / workspace-write / danger-full-access），live 追加持久 session-log 事件（`permission/preset`+`sandbox/mode`，重启/恢复后仍生效）——所以 dsh 不再走「持久化 + kill + 重启」路径，而是直接驱动 TUI 命令（`Composer.tsx:461-482`）。
- 换模型：**dsh 不支持原位换模型**——`/model` 是会话 fork 续聊（历史保留、新会话路由新模型、旧会话留在 `/resume`），会破坏「tab id = session id」身份。Composer 只持久化配置，由下一次 spawn / resume 经 `--patch` 钉死生效，不驱动运行中的 TUI（`Composer.tsx:589-604`）。
- 思考强度：**Shift+Tab**（`ESC[Z`）环 `off → high → max`（dsh-tui 的 effort 循环，deepseek 档位）。CaPilot speed 映射 fast→off / mid→high / high→max，auto=省略（profile 默认 max）；按当前持久化档位到目标档位的环距步数驱动（`Composer.tsx:673-696`）。**仅 deepseek-official 路由可调**：pi-ai 路由（opencode-go 等）只持久化 speed（作为下次 deepseek-official 生成的默认档位）、不驱动 TUI；⚡ 菜单只留 auto/off（`Composer.tsx:327-340`）。

**Resume / 会话**：
- `DSH_TUI_RESUME_SESSION=<session-id>` env（无 argv；`dsh.rs:598-600`，`resume_args` 返回空）。0.7.2 起 canonical 名改为 `DSH_TUI_*`，CaPilot 同时设旧 `DSH_CC_RESUME_SESSION` 兼容旧 dsh 二进制。
- resume key = 10 秒内新会话目录名（`$DSH_HOME/sessions/--<projectKey(cwd)>--/<session-id>/session.jsonl[.zstd]`，mtime 窗口 + header `cwd` 匹配）；回退 `~/.dsh-tui/resume.txt`（0.7.2 launcher 双写 `~/.dsh-tui` + 旧 `~/.dsh-cc`，读取按 canonical→legacy 顺序，用户 `/exit` 时 launcher 写的标记，仅 fallback）（`dsh.rs:388-421`）。
- projectKey 编码：cwd 的每个非 `[a-zA-Z0-9]` 字符 → `-`（含前导 `/`），去首尾 `-`、空则 `root`、最长 251 字符（`dsh.rs:78-99`）。
- 会话日志：默认 zstd 压缩（`session.jsonl.zstd`，`ruzstd` 解码；`session.jsonl` 明文变体直接读）（`dsh.rs:177-197`）。

**状态上报（方案 B：Rust tail JSONL 推断，无 hook）**：
- dsh **没有** shell hook 面（非 claude `--settings` / codex config profile / opencode 插件型）。CaPilot 用 Rust 侧推断：`agent_status_read` 对 dsh 走 DB 拿 runtime+cwd → `newest_session_log_meta`（路径 + mtime_ns + len 指纹）→ 指纹命中 `StatusInferenceCache` 直接复用，未命中才 `spawn_blocking` 解码 zstd + `infer_status_from_content`（`lib.rs` `agent_status_read`、`StatusInferenceCache`）。
- 推断规则：JSONL 逐行，`turn/start`→working、`turn/end`→idle、`assistant/chunk`→working，**最后一条命中事件胜出**（`dsh.rs` `infer_status_from_content`）。ts = 日志 mtime 秒。
- 与 hook 侧车同构：`HOOK_STATUS_RUNTIMES` 含 dsh，TabBar 1s 轮询同一 `agent_status_read` 路径（`TabBar.tsx:46-49`）。
- 缓存：`StatusInferenceCache` 以 `(mtime_ns, len)` 为指纹（mtime 被文件系统取整到整秒也能靠 len 捕获追加的 `turn/end`），不变则轮询零解码。

**上下文占用 + 缓存命中数据源**：`assistant/chunk` usage 事件——used = `inputTokens + cacheReadTokens`（DeepSeek 会计把输入拆成 fresh 与 cache-served 两部分；实测真实日志二者分离、cacheRead 随上下文累积单调增长；结尾 `{inputTokens:0, outputTokens:0}` 重置块跳过）；max = 模型清单 `context_window_max`（flash/pro 均为 1M，未知模型不猜）。**会话累计**命中率分子/分母 = 所有 usage 事件的 `cacheReadTokens` 与 `inputTokens + cacheReadTokens`（`dsh.rs:622-638`、`parse_usage_from_content`、`context_window_max`）。

### 2.5 pi（runtime `pi`，@earendil-works/pi-coding-agent，本机 0.84.x）

> 适配器 `pi.rs`；Composer 对 pi 有专属 TUI 分支：模型经 **Ctrl+L 模型选择器**、思考经 **Shift+Tab 环**原位驱动（见下 **Live 控制**）；**权限 live 切换 = 持久化 + kill + 自动恢复重启**——pi 无 live 批准键，launch flag（`--approve`/`--no-approve`）在进程启动时定死，`/reload` 只重读 trust store、不重新解析 trust。pi 是 npm 分发、`dist` 源码即文档；本地权威 = `pi --help` / `pi --list-models`。

**Launch**（`pi.rs:358-375`）：
```text
pi [--provider <p>] [--model <provider/id>] [--thinking off|low|medium|high|xhigh] [--approve|--no-approve]
```
- 模型：provider-qualified `<provider>/<model>`，目录来自 `pi --list-models`（`PI_OFFLINE=1` 走本地 bundle catalog，`pi.rs:129-190`）；default = `<config>/settings.json` 的 `defaultProvider`/`defaultModel`（`pi.rs:103-113`）。launch flag 钉死**下一次 spawn 的默认**；**运行中的会话**可经 Ctrl+L 模型选择器原位切换（见下）。
- 权限映射 `ask→无 flag（TUI 询问）`、`auto→--approve`、`yolo→--approve`（yolo `requires_confirmation: true`）。**pi 无 sandbox**：approve 只决定文件/命令是否免确认，不提供只读/工作区写隔离（`pi.rs:301-327`）。
- 思考强度：CaPilot speed → `--thinking`：`off→off`、`fast→low`、`mid→medium`、`high→high`、`xhigh→xhigh`、`auto→省略`（`pi.rs:191-203`、`442-448`）。

**Live 控制（PTY 注入）**：
- **权限 → live 靠重启**（`Composer.tsx:540-556` `applyPermissionMode` pi 分支）：pi 无 live 批准键（无 `/permissions`；`--approve`/`--no-approve` 仅 launch flag；`/trust` 写持久 per-cwd 决定且需重启；`/reload` 重读 trust store 但**不**重新解析 trust——`handleReloadCommand` 的 `session.reload({ beforeSessionStart })` 不传 `resolveProjectTrust`）。因此 live 会话切换权限 = 先 `agent_set_session_config` 持久化新 mode → `agent_kill` → `dropAgentChannel` → XTermPanel channel effect（deps `[agentId, channel]`）见 channel 归 null 自动 `agent_resume`，`AgentNotFound` 落到 `build_and_spawn` 用新 mode 重启（`lib.rs:607-627`）。休眠/已结束会话无 live 进程，只持久化即可（下次打开/恢复生效）。注意 `auto→--approve` 与 `yolo→--approve` 在 pi 下等价（pi 无更高权限档），可见差异只在 ask↔auto 之间。
- **模型 → live**（`Composer.tsx:671-680` `applyModel` pi 分支）：`ctrl+l`（0x0C）开模型选择器，其搜索框**聚焦即输入**；搜索文本含 `provider/id`（`${provider} ${provider}/${id} ${provider} ${id}${name}`），输入 provider 限定的目录 id 让目标模型排第一，`\r` 选中（无需方向键）。选择器已打开时不会重复开；非 pi 分支先经 `ensureAgentChannel` 恢复（pi 分支保留恢复，切换需 PTY）。
- **思考 → live**（`Composer.tsx:797-831` `applyThinkingSpeed` pi 分支）：`shift+tab`（`ESC[Z`）环 `off → minimal → low → medium → high → xhigh → max`（模型可跳过不支持的档）。当前档读 `pi_current_thinking_level`（`pi.rs:116-126` `current_thinking_level` → `<config>/settings.json` `defaultThinkingLevel`，pi 在每次 live 变更时下一 tick 写回），每次按键后重读——环距步进到目标即停，clamp 不会过冲。`auto` = 当前档（=默认），无操作。⚡ 菜单用完整档位（pi `list_thinking_options`：auto/fast/mid/high/xhigh）。

**Resume / 会话**：
- `pi --session <key>`（`pi.rs:376-382`）；key = pi 会话的 uuidv7 id，来自会话 JSONL 文件名 `<UTC时间戳>_<uuidv7>.jsonl`（`pi.rs:190-197`）。
- 会话目录：`<config>/sessions/--<projectKey(cwd)>--/`；config 默认 `~/.pi/agent`（`$PI_CODING_AGENT_DIR` 覆盖，会话目录另可用 `$PI_CODING_AGENT_SESSION_DIR`）。projectKey = cwd 去前导 `/`、`/ \ : → -`、包在 `--…--`（`pi.rs:47-55`，测试 `project_session_dir_matches_pi_encoding`）。
- `capture_resume_key`：cwd 对应会话目录里 10 秒内最新的 `.jsonl` 的 id（`pi.rs:388-411`）。`context_usage` 用持久化 `resume_key` 钉死精确文件、无 key 时回退最新文件（`pi.rs:413-426`），与 claude/codex/opencode/dsh 同一身份规则。

**状态上报（无 hook，PTY 活动启发式）**：
- pi **没有** hook 面（非 claude `--settings` / codex config profile / opencode 插件 / dsh JSONL 推断型）。`agent_status_read` 对 pi 返回 `None`，`HOOK_STATUS_RUNTIMES` 不含 pi（`TabBar.tsx:49`），TabBar 用 PTY 活动启发式（同 bash）。

**上下文占用 + 缓存命中数据源**：`type:"message"` 助手消息的 `message.usage`（camelCase `input`/`output`/`cacheRead`/`cacheWrite`/`totalTokens`）。used = 最后一个非零 usage 的 `input + cacheRead + cacheWrite`；**会话累计**命中率分子/分母 = 所有助手 usage 的 `cacheRead` 与 `input + cacheRead + cacheWrite`（`pi.rs:233-278` `usage_from_content`）；`actual_model` = 最后一条 `message.model`（仅展示）。max = **None**——pi 会话 JSONL 不携带 context window、模型目录也未暴露上下文上限，不猜（`pi.rs:413-426`）。

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
| 21 | dsh launch 方式 | `dsh --profile dsh-tui --patch <每会话临时文件>`；**无 argv 注入模型/权限/effort**——模型+effort 经 `--patch` overlay（整体替换 dsh-tui 配置行），权限经 env `DSH_PERMISSION_MODE=read-only\|workspace-write\|danger-full-access`；`NODE_ENV=production` 必须（dev renderer OOM）。patch 落 `$XDG_CACHE_HOME/capilot-ide/dsh/<safe-id>.patch.yml`，会话删除时清理 | `dsh.rs` `write_patch`/`spawn_interactive`/`launch_env`/`dsh_permission_mode` | dsh-tui 插件 README（本地 `~/.dsh/profiles/dsh-tui` 副本） |
| 22 | dsh 思考档位 | **Shift+Tab**（`ESC[Z`）环 `off → high → max`（dsh-tui effort 循环，仅 deepseek-official 路由）；speed 映射 fast→off / mid→high / high→max，auto=省略（profile 默认 max）；**pi-ai 路由固定 `effort: off`**、⚡ 菜单只留 auto/off（防 UNSUPPORTED_REASONING_EFFORT） | `dsh.rs` `effort_for_speed` + `Composer.tsx:673-696`/`327-340`（环距步数驱动） | 实测 dsh-tui 循环 |
| 23 | dsh 模型换法 | **不支持原位换模型**：`/model` 是会话 fork 续聊（新会话路由新模型、旧会话留 `/resume`），会破坏「tab id = session id」身份。Composer 只持久化配置，下次 spawn / resume 经 `--patch` 钉死生效 | `Composer.tsx:589-604`（dsh 分支空操作）+ `dsh.rs` `write_patch` | dsh-tui README（model 命令语义） |
| 24 | dsh resume / 会话 | `DSH_TUI_RESUME_SESSION=<id>` env（无 argv；0.7.2 canonical，CaPilot 同时设 `DSH_CC_RESUME_SESSION` 兼容旧 dsh）；resume key = 10 秒内新会话目录名（`$DSH_HOME/sessions/--<projectKey>--/<id>/session.jsonl[.zstd]`，header `cwd` 匹配），回退 `~/.dsh-tui/resume.txt`（launcher 双写，`~/.dsh-cc` 旧文件仅兼容读取）。patch `sessionId: !!js process.env.DSH_TUI_RESUME_SESSION ?? process.env.DSH_CC_RESUME_SESSION ?? undefined` 必须**无条件重写**（漏写丢 resume 缝） | `dsh.rs` `capture_resume_key`/`detect_recent_resume_key`/`write_patch`/`launch_env` + `lib.rs` `agent_resume` | 实测 dsh 会话目录 |
| 25 | dsh hook 注入方式 | **无 hook 面**（非 claude/codex/opencode 型）。方案 B：Rust 侧 tail JSONL 推断（`turn/start`→working、`turn/end`→idle、`assistant/chunk`→working，**最后一条命中胜出**），`agent_status_read` 对 dsh 走 DB(runtime+cwd)→`newest_session_log_meta`(mtime_ns+len 指纹)→`StatusInferenceCache` 命中复用、未命中 `spawn_blocking` 解码+推断 | `lib.rs` `agent_status_read`/`StatusInferenceCache` + `dsh.rs` `infer_status_from_content`/`infer_status` | 实测 dsh session.jsonl.zstd 结构 |
| 26 | dsh 上下文占用 + 缓存命中数据源 | 按 `resume_key`（会话子目录名）**钉死精确 session**，无 key 时回退 cwd 下最新会话（防同 cwd 兄弟会话串数据，与 claude/codex/opencode 同一身份规则）。`assistant/chunk` usage 事件：used = `inputTokens + cacheReadTokens`（DeepSeek 把输入拆 fresh/cache 两部分，cacheRead 随上下文单调增长；结尾 `{0,0}` 重置块跳过）；max = 模型清单 `context_window_max`（flash/pro 均 1M，未知不猜）。**会话累计**命中率分子/分母 = 所有 usage 事件的 `cacheReadTokens` 与 `inputTokens + cacheReadTokens`；`request/header` 的 route model 作为 `actual_model` 上报（仅展示） | `dsh.rs` `context_usage`/`parse_usage_from_content`/`context_window_max`/`session_log_for_key` | 实测 dsh session.jsonl.zstd（usage 事件） |
| 27 | pi launch 方式 | `pi [--provider <p>] [--model <provider/id>] [--thinking <level>] [--approve\|--no-approve]`；模型/思考/权限都走 launch flag，无 `--patch` 式 overlay。default model = `<config>/settings.json` 的 `defaultProvider`/`defaultModel` | `pi.rs` `spawn_interactive`/`default_model` | `pi --help`（本地 npm 包） |
| 28 | pi 思考档位 | CaPilot speed → `--thinking`：`off/fast/mid/high/xhigh → off/low/medium/high/xhigh`，`auto`=省略 flag；**运行中**经 Shift+Tab 环原位切换（行 33） | `pi.rs` `thinking_for_speed`/`speed_args` | `pi --help` |
| 29 | pi 权限 flag | `ask→无 flag`、`auto→--approve`、`yolo→--approve`；**无 sandbox**（approve ≠ 沙箱隔离） | `pi.rs` `mode_args`/`list_permission_modes` | `pi --help` |
| 30 | pi resume / 会话 | `--session <uuidv7>`；会话目录 `<config>/sessions/--<projectKey(cwd)>--/`，文件名 `<UTC时间戳>_<uuidv7>.jsonl`；projectKey = cwd 去前导 `/` + `/ \ : → -`；`$PI_CODING_AGENT_DIR`/`$PI_CODING_AGENT_SESSION_DIR` 可覆盖 config/会话目录 | `pi.rs` `capture_resume_key`/`project_session_dir`/`session_id_from_file_name` | 实测 `~/.pi/agent/sessions` |
| 31 | pi hook 注入方式 | **无 hook 面**（非 claude/codex/opencode/dsh 型）。`agent_status_read` 对 pi 返回 `None`，状态退回 PTY 活动启发式（同 bash） | `pi.rs`（无 `status_hook_args`）+ `TabBar.tsx:49`（`HOOK_STATUS_RUNTIMES` 不含 pi） | 实测 pi 无 settings/plugin/JSONL 推断缝 |
| 32 | pi 上下文占用 + 缓存命中数据源 | 按 `resume_key` 钉死精确会话 JSONL，无 key 回退最新文件。`message.usage` camelCase（`input`/`output`/`cacheRead`/`cacheWrite`/`totalTokens`）；used = 最后非零 `input+cacheRead+cacheWrite`；**会话累计**命中率分子/分母 = `cacheRead` 与 `input+cacheRead+cacheWrite`；`actual_model` = 最后一条 `message.model`；max = **None**（会话 JSONL 与模型目录都不暴露 context window，不猜） | `pi.rs` `context_usage`/`usage_from_content` | 实测 pi 会话 JSONL（usage 事件） |
| 33 | pi live TUI 键位 | **Ctrl+L**（0x0C）= 模型选择器（搜索框聚焦即输入，搜索文本含 `provider/id`，Enter 选中首个过滤结果）；**Shift+Tab**（`ESC[Z`）= 思考档循环 `off→minimal→low→medium→high→xhigh→max`（模型可跳档）；当前思考档读 settings.json `defaultThinkingLevel`（pi 每次变更下一 tick 写回） | `Composer.tsx:653-662`/`779-813` + `pi.rs` `current_thinking_level` | pi dist 源码 keys.js/model-selector.js + `~/.pi/agent/settings.json` |
| 34 | pi 权限 live 面 | **无 live 批准键**：`--approve`/`--no-approve` 仅 launch flag，`/trust` 写持久 per-cwd 决定且需重启，`/reload` 重读 trust store 但不重新解析 trust。Composer 权限切换 = 持久化 + kill + 自动恢复重启（live 会话），休眠会话仅持久化（`applyPermissionMode` pi 分支） | `Composer.tsx:540-556` | pi dist 源码 keys.js（无 approval 绑定）、interactive-mode.js `handleReloadCommand` |

> 编号 1、5、8、20、22、**33** 都是「顺序/键位」型事实，**最容易随版本漂移**。改它们时，确认当前 TUI 实际行为后再动代码，别只凭旧文档。22 是 dsh 侧同样的顺序型事实（Shift+Tab 环）、33 是 pi 侧（Ctrl+L 模型选择器 + Shift+Tab 思考环）——改前先确认对应 TUI 实际循环。pi 是年轻 CLI（0.84.x、周更级迭代）——27-29 的 launch flag 面与 33 的键位面最容易漂移，改前以 `pi --help` / `pi --list-models` 实测为准。

**OpenCode 首轮低命中基线（1.18.18 / Zen DeepSeek V4 Flash Free，2026-08-15 实测）**：两个全新会话的首轮分别为 `input=8886, cache_read=1792`（16.78%）和 `input=8370, cache_read=1792`（17.63%）。固定的 1792 说明首轮大概率只复用了 provider 可识别的公共前缀；OpenCode/Zen 只返回 token 数量，不返回命中的具体文本区间，因此不能进一步断言是哪一段。Build agent 首轮还会带上 system prompt、工具 schema、权限、环境和项目规则；CaPilot-Ide 会额外加载 2,216 字节的 `CLAUDE.md`，两个项目的非缓存 input 相差 516 tokens（路径与环境差异也可能占一部分）。同一台机器上的长工具循环会话累计命中率可达到 92.05%，所以首轮低值不能通过改公式“修高”，应保留 provider 原始会计语义并把它视为冷启动基线。

**缓存命中率 chip（composer target 行）**：`AgentUsage` 新增 `cache_hit_tokens` / `cache_total_input_tokens` 两个**会话累计**字段（wire camelCase，`adapter.rs` `AgentUsage`）。各 adapter 按自己 runtime 的会计语义归一化后再上报（Claude/opencode：分子 `cache_read`、分母 `input+cache_read(+cache_write/creation)`；codex：分子 `cached_input_tokens`、分母 `input_tokens`），前端只算 `cache_hit/cache_total` 比例、不做跨 runtime 换算。只有两者都在且分母 > 0 才渲染 chip（`ui/components/layout/CacheHitRate.tsx`）；bash 等无 `context_usage` 的 runtime 永不渲染。

新增 Provider 必须通过 context-window-usage 的 identity、隔离、0 值、去重/聚合边界、实际模型和双会话回归清单；只在单会话 happy path 上能显示数字不算完成。

---

## 4. 保持新鲜

- 本文件事实核对日期 **2026-08-16**（新增 pi 集成 + dsh-tui 0.7.2 复核：env 名/数据目录 rename、resume 双读、sessions 根不变；claude/codex/opencode 条目保持 08-15 核对）。
- 五个 runtime 官方文档都在周更级迭代。改 adapter 前：
  1. 先在 §1 的对应子页确认最新 flag/键位/顺序；
  2. 更新 §3 表格与对应代码锚点；
  3. 若 `claude`/`codex`/`opencode` CLI 已安装到本机，可 `--help` / 实测 TUI 行为做最终校验（本机 UI 自动化受限，见运行手册）。dsh 同理：`dsh --help` 只见 launcher 面，TUI 行为以 `dsh-tui` 插件 README（`~/.dsh/profiles/dsh-tui`）与实测会话日志为准。
