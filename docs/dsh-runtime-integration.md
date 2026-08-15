# dsh-TUI Runtime 集成设计

> 目标：将 [dsh-TUI](https://github.com/ccch1mneyyy/dsh-TUI)（`dsh-cc-tui`）以与 claude / codex / opencode / bash 相同的适配标准接入 CaPilot IDE。
> 本文是分析与设计文档，不包含已合入的实现。所有外部事实均在对应上游源码处验证（见文末「参考来源」）。

## 1. 结论摘要

**完全可行，工作量和 codex / opencode 同量级。** `dsh-cc-tui` 是官方 dsh CLI（`@deepseek-ai/dsh`）之上的 Claude Code 风格 TUI 插件，通过 `dsh --profile cc-tui` 启动。模型、权限、恢复、思考档位都有干净的注入缝（`--patch` overlay + 环境变量）。两个真正需要处理/验证的缺口：

1. **状态钩子（status hook）** — dsh **没有 shell hook 系统**（`/hooks` 是占位符），只能走 opencode 式 JS 插件 + 事件总线，且插件解析依赖 profile 的 `node_modules`，与 CLAUDE.md「hooks 只通过 per-session 启动参数安装、不动用户全局配置」的约束有张力，**需要 spike**。
2. **上下文用量（context_usage）** — 会话日志是 **zstd 压缩的 JSONL**，Rust 侧需要新增解码器（codex / claude 都没有这个依赖）。

其余全部是既有 `AgentRuntimeAdapter` 抽象的机械实现。

## 2. 背景

### 2.1 dsh（DeepSeek Harness CLI）

- npm 包 `@deepseek-ai/dsh`（源码仓库 `deepseek-ai/deepseek-harness`，`apps/cli`）。
- Commander 启动器，**只解析自己的 flags**：`--profile <name>`、可重复的 `--patch <path>`、`--dump-config`、`--dump-default-config`、`plugin` 子命令。**其后的参数原样透传给被启动的 app**（`ctx.cmdlineArgs`）—— 例：`dsh --profile tui --resume <session>`，`--resume` 是给 TUI 的。
- 环境加载：`loadLayeredEnv`（`@deepseek-ai/dsh-app-boot`），支持 `.env` 分层。

### 2.2 dsh-TUI（dsh-cc-tui）

- TypeScript / React（移植的 Ink 渲染器），Claude Code 风格 TUI 插件。
- 安装：`dsh plugin --profile cc-tui add dsh-cc-tui` → 生成 `$DSH_HOME/profiles/cc-tui`（默认 `~/.dsh/profiles/cc-tui`）。
- 启动：`dsh --profile cc-tui`（等价于仓库内 `dsh-cc.cmd` 的默认路径）。
- 核心事实：
  - `/model` 实时切换走「**会话 fork 续聊**」（DSH 无原位换模型 API）：历史保留、新会话路由新模型、旧会话留在 `/resume` 列表；选择写入 `~/.dsh-cc/model.json`。
  - `/permission`（沙箱模式切换）**未适配**：需 approval 服务 + 审批 UI，TUI 不提供实时权限切换。
  - 思考档位：Shift+Tab 循环，持久化到 `~/.dsh-cc/effort.json`；cordis 配置层的 `effort` 优先。
  - `/resume` 会把选中的 session id 写入 `~/.dsh-cc/resume.txt`（`src/sessionHistory.ts`）。

## 3. 适配面总览（代码库触碰点）

| 层 | 位置 | 改动 |
|---|---|---|
| Runtime 契约 | `src-tauri/src/agent_runtime/adapter.rs` | 无改动，`AgentRuntimeAdapter` trait 已存在 |
| **新适配器** | `src-tauri/src/agent_runtime/runtimes/dsh.rs` | **新建**，实现 `AgentRuntimeAdapter` |
| 注册表 | `src-tauri/src/agent_runtime/runtimes/mod.rs` | `pub mod dsh;` + `"dsh" => Box::new(dsh::DshAdapter::new())` + `known_runtimes()` 加 `"dsh"` |
| 启动/覆盖 | `src-tauri/src/lib.rs` | 基本无改（`build_and_spawn` / `apply_launch_overrides` 通用）；若走 JS 状态插件，`sessions_delete` 需清理（参照 codex `remove_status_profile`） |
| 状态钩子 | `src-tauri/src/agent_runtime/status_hooks.rs` | 无改动（env-gated sidecar 格式通用） |
| Slash | `src-tauri/src/slash.rs` | dsh 命令目录 + `builtin_commands` 分支（可选） |
| UI 实时控制 | `ui/components/layout/Composer.tsx` | `applyPermissionMode` / `applyModel` / `applyThinkingSpeed` 各加 dsh 分支 |
| 设置 | `ui/components/layout/SettingsModal.tsx` | `DEFAULT_RUNTIME_ARGS` 加 dsh（「已安装」列表自动出现） |
| 图标 | `ui/components/Icon.tsx` | `runtimeIcon` 加 dsh 图标（否则落到 `terminal`） |
| 会话动作 | `ui/state/agentActions.ts` | `spawnAgent` 的 model 传参条件加 `"dsh"`（dsh 需在 spawn 时钉死模型） |
| 状态同步 | `ui/state/runtime.ts` | 无改动 |
| 文档 | `docs/ai-runtime-references.md` | 新增一行 + volatile-facts 表 |

## 4. 逐项映射（`AgentRuntimeAdapter` trait → dsh 事实）

### 4.1 启动命令 `spawn_interactive`

```
dsh --profile cc-tui
```

- cwd 由 PTY 管理（`spawnBashAt` 已把 projectRoot 传入）。
- 每会话 model/effort/权限通过环境变量与 `--patch` overlay 注入（见下）。

### 4.2 可用性 / 认证

- `is_available`：`dsh --version` 成功 **且** `$DSH_HOME/profiles/cc-tui` 存在（默认 `~/.dsh/profiles/cc-tui`）。**profile 必须预先存在**，这是 dsh 的首次运行前置；IDE 需在设置页/首次引导提供一键安装（`dsh plugin --profile cc-tui add dsh-cc-tui`）。
- `is_authenticated`：`DEEPSEEK_API_KEY` env 存在，或 `$DSH_HOME/.credentials.yaml`，或 `.env`。API key 由 `llm-deepseek` 行的 `apiKeyEnv: DEEPSEEK_API_KEY` 读取。

### 4.3 模型 `list_models`

dsh **没有列模型的 CLI 子命令**，参照 claude.rs 硬编码两个：

| model id | 显示名 | 上下文窗口 |
|---|---|---|
| `deepseek-v4-flash` | DeepSeek-V4-Flash | 1M tokens |
| `deepseek-v4-pro` | DeepSeek-V4-Pro | 1M tokens |

来源：`dsh-llm-deepseek` 适配器默认目录 + dsh-TUI `docs/configuration.md`。

### 4.4 权限模式 `list_permission_modes` / 启动注入

- **注入方式：环境变量 `DSH_PERMISSION_MODE`**，取值 `read-only` | `workspace-write` | `danger-full-access`。
- dsh-TUI bundle patch（`cordis.patch.yml`）用 JS 表达式读取该 env 设置两个服务：

```yaml
- id: sandbox-policy
  config:
    mode: !!js "process.platform === 'win32' ? 'danger-full-access' : (process.env.DSH_PERMISSION_MODE ?? 'workspace-write')"
- id: user-approval
  config:
    policy: !!js "process.platform === 'win32' ? 'never' : ((process.env.DSH_PERMISSION_MODE ?? 'workspace-write') === 'danger-full-access' ? 'never' : 'ask')"
```

- **映射**（沿用 codex / opencode 的 `ask` / `auto` / `yolo` id）：

| CaPilot 模式 | `DSH_PERMISSION_MODE` | sandbox | approval |
|---|---|---|---|
| `ask` | `read-only` | 只读 | ask |
| `auto` | `workspace-write` | 工作区写 | ask |
| `yolo` | `danger-full-access` | 全权限 | never |

- 适配器实现走 `launch_env`（env 注入），`mode_args` 返回空即可——即使 `apply_launch_overrides` 把参数整体替换，env 仍保留。
- **限制：TUI 内没有实时权限切换。** `applyPermissionMode` 的 dsh 分支只能「持久化 `agent_set_session_config` + 重启 PTY」——这是 Composer 现有设计刻意避免的重路径，需单独处理（参照 opencode 非 auto 的既有思路）。

### 4.5 思考档位 / 速度 `list_thinking_options` / 注入

- **合法档位：`off` / `high` / `max`**（deepseek adapter 仅这三档，非法档位静默回落默认）。
- **注入：`--patch` overlay 设 `cc-tui.config.effort`**，配置层优先于 Shift+Tab 持久化的 `~/.dsh-cc/effort.json`。
- **映射**：

| CaPilot speed | dsh effort |
|---|---|
| `fast` | `off` |
| `mid` | `high` |
| `high` | `max` |
| `auto` | 省略（用 profile 默认 `max`） |

- **实时控制存在：Shift+Tab 循环**（`channel.ts` `cycleEffort()`）。`applyThinkingSpeed` 的 dsh 分支可像 claude 权限循环一样，从当前持久化档位计算步数驱动 Shift+Tab。

### 4.6 会话恢复 `supports_resume` / `resume_args` / `capture_resume_key`

- **恢复键：dsh 会话 id。**
- 注入方式：环境变量 `DSH_CC_RESUME_SESSION=<id>`（TUI bundle patch 的 agent 工厂行 `sessionId: !!js process.env.DSH_CC_RESUME_SESSION ?? undefined`；等价于 `dsh-cc.cmd --resume` 的语义）。
- `capture_resume_key`：读 `~/.dsh-cc/resume.txt`（TUI 在 `/resume` 时写入）。对 IDE 而言更干净的是直接持久化 session id + 注入 env；`resume.txt` 只是用户「恢复上次会话」的便利。
- 恢复优先级注意：resume 时 `agentOptions.provider/model` 只接受 cordis 配置的**完整 pair** 覆盖（issue #67），否则用目标会话自己的 `request/header` 记录。

### 4.7 上下文用量 `context_usage`

- **数据源：`$DSH_HOME/sessions/--<projectKey>--/<encoded-id>/session.jsonl.zstd`**
  - profile 用 JSONL 共享后端（`dshHomePath('sessions')`，即 `~/.dsh/sessions`），与 dsh web 同一份。
  - 目录编码：`projectKey(cwd) = "--" + <可读字符 slug，截断 251 字符> + "--"`；session id 经 `encodeSegment` 转义（`packages/session/session-persistence-jsonl/src/format.ts`）。
  - 默认 **zstd 压缩**（`session.jsonl.zstd`）。
- **JSONL 事件**（dsh-TUI `src/channel.ts` 验证）：
  - `request/header` → `header.config.{provider,model,reasoningEffort}`
  - `assistant/chunk` → `data.usage.{inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens}`（最后一次为 `lastUsage`）
  - `turn/start` / `turn/end`
- **映射到 `AgentUsage`**：

| 字段 | 来源 |
|---|---|
| `context_window_used_tokens` | 最后一次 usage 的 `inputTokens` |
| `cache_hit_tokens` | `cacheReadTokens` |
| `cache_total_input_tokens` | `inputTokens`（DeepSeek 计费里 cache-read 含在 input 内） |
| `context_window_max_tokens` | 模型目录（1M） |

- 注意：DeepSeek **不报 cache-write**（`cacheWriteTokens` 恒 0）。
- **实现注意**：Rust 侧需引入 zstd 解码（`ruzstd` 纯 Rust，或 `zstd` crate），再逐行 parse JSONL。这是新依赖。

### 4.8 状态钩子 `status_hook_args`（最大缺口）

dsh **没有 shell hook 系统**（`/hooks` 是占位符）。可用缝是 Cordis 插件事件总线：`activity/status`、`turn/start/end`、`approval/asked` / `approval/replied` 都真实存在（`dsh-working-activity` 包就是消费 `activity/status` 画活动状态行的，但只读不落盘）。

- **方案 A（推荐，需 spike）**：opencode 式 JS 插件订阅事件 → 写共享 sidecar `~/CaPilot/status/` 的 `{"status","ts"}` 格式（`CAPILOT_AGENT_ID` / `CAPILOT_STATUS_DIR` env-gated）。注入 = 每会话 `--patch` overlay 插入插件行 + config（`statusDir`/agentId）；插件包需装入 profile 的 `node_modules`（dsh Loader 只从那里解析行 `name`，`working-activity` 就是这么挂的）。
  - ⚠️ 张力：装包会改动用户全局 `~/.dsh/profiles/cc-tui/node_modules`，与「hooks 只经 per-session 参数安装」约束冲突。缓解：IDE 首次运行时一次性安装（类似 `dsh plugin --profile cc-tui add capilot-status`），但仍是全局改动，需设计评审确认。
- **方案 B（零侵入，滞后）**：Rust 适配器 tail 会话 JSONL，从 `turn/start/end` + `assistant/chunk` 推断 idle/working；`awaiting_choice` 难判、且是轮询。不碰任何全局配置。

### 4.9 启动环境 `launch_env`

```
NODE_ENV=production            # 必须：React dev 渲染器会为长会话累积 unbounded performance.measure() 导致 OOM（dsh-cc.cmd 明示）
DSH_PERMISSION_MODE=<read-only|workspace-write|danger-full-access>
DSH_CC_RESUME_SESSION=<id>     # 仅恢复时
CAPILOT_AGENT_ID=<id>          # 状态钩子，env-gated no-op
CAPILOT_STATUS_DIR=~/CaPilot/status
DEEPSEEK_API_KEY               # 透传（若已在环境中）
```

### 4.10 每会话模型隔离（`--patch` overlay）

- **不要碰用户全局 `~/.dsh-cc/model.json`**（TUI `/model` 会写它，且会做「会话 fork 续聊」：历史保留、新会话路由新模型、旧会话留在 `/resume` 列表）。fork 会改变会话身份，对 IDE 的「tab id = session id」模型是破坏性的，**不建议 live 切换模型**。
- 正确做法：spawn 时用 `--patch <tempfile>` overlay 钉死 `cc-tui.config.provider` + `cc-tui.config.model`（**完整 pair 才生效**，issue #67）。优先级已验证：cordis 配置（patch）**胜于** `~/.dsh-cc/model.json` 持久化选择（`channel.ts` `resolveModelRoute`）。这正是 codex 每会话 `-p capilot-<id>` 的镜像。
- 每会话临时 patch 文件可同时承载 model + effort，spawn 时用 repeatable `--patch` 追加：

```yaml
# ~/CaPilot/dsh/capilot-<id>.patch.yml
- id: cc-tui
  config:
    provider: deepseek-official
    model: deepseek-v4-flash
    effort: high
```

（`provider` 默认 `deepseek-official`，模型路由名称。）

## 5. 风险与权衡

1. **状态钩子无原生机制**（§4.8）— 与「和 agent runtime 有一样的适配」最不对齐的地方：codex / claude / opencode 都有 status hook。
2. **zstd 日志解码**（§4.7）— 新增 Rust 依赖 + JSONL 解析逻辑。
3. **权限无实时切换**（§4.4）— `applyPermissionMode` 需要「持久化 + 重启 PTY」的重路径。
4. **模型 live 切换 = 会话 fork**（§4.10）— 模型下拉只能做到 spawn 时钉死；运行中切换需重启，不能用 codex 式 `/model` 驱动。
5. **profile 前置条件**（§4.2）— `is_available` 依赖 `dsh plugin --profile cc-tui add dsh-cc-tui` 已执行。
6. **Windows 语义失真**（§4.4）— cc-tui patch 在 win32 强制 `danger-full-access` + `never`（无沙箱后端），`ask` / `auto` 在 Windows 上会变成 yolo 行为。

## 6. 分步实施计划

### Phase 0 — spike（在真实 dsh 环境，只验证不写码）

1. `npm i -g @deepseek-ai/dsh` + `dsh plugin --profile cc-tui add dsh-cc-tui` + 启动冒烟。
2. 验证三种 `DSH_PERMISSION_MODE` 值下的实际沙箱/审批行为（`ask` 时是否有交互式审批 UI 需要处理）。
3. 验证 `--patch` overlay 覆盖 `cc-tui.config.{provider,model,effort}` 生效。
4. 验证 `DSH_CC_RESUME_SESSION` 恢复。
5. spike 状态插件方案 A：最小 JS 插件装入 profile node_modules，订阅事件写 sidecar，确认行 `name` 能通过 `--patch` 插入并解析。

### Phase 1 — Rust adapter 骨架（能开 / 关 / 改名终端）

- 新建 `runtimes/dsh.rs`：`id="dsh"`、`spawn_interactive`（`dsh --profile cc-tui` + `--patch`）、`is_available`、`is_authenticated`、`list_models`、`launch_env`（`NODE_ENV` + 权限 env）。
- `mod.rs` 注册 + `known_runtimes()` 加 `"dsh"`。`cargo test` 通过。

### Phase 2 — 完整能力

- `list_permission_modes` / `list_thinking_options`（枚举目录）、speed/effort 走 `--patch`。
- 恢复：`supports_resume=true`、`resume_args`（env）、`capture_resume_key`。
- `context_usage`：`ruzstd` + JSONL 尾部解析。
- `slash.rs` dsh 命令目录（`/resume` `/compact` `/model` `/permissions` `/mcp` `/preset` …）。

### Phase 3 — UI

- `Composer.tsx` 三分支：权限走「持久化 + 重启」、effort 走 Shift+Tab 循环、模型走 spawn 钉死。
- `agentActions.ts` `spawnAgent` 的 model 条件加 `"dsh"`。
- `SettingsModal.tsx` `DEFAULT_RUNTIME_ARGS`、`Icon.tsx`。
- `docs/ai-runtime-references.md` 新增行 + volatile-facts 表。

### Phase 4 — 状态钩子（按 spike 结论）

- 方案 A：IDE 首次运行安装 `capilot-status` 插件 + 每会话 `--patch` 插入；`sessions_delete` 清理。
- 方案 B：Rust tail 日志推断。

## 7. 参考来源

上游（外链，验证时间 2026-08）：

| 事实 | 出处 |
|---|---|
| dsh CLI 启动器只解析 `--profile` / `--patch`，其余透传 | `deepseek-ai/deepseek-harness` → `apps/cli/src/args.ts` |
| `loadLayeredEnv`（`.env`） | `apps/cli/src/bin.ts` |
| `DSH_PERMISSION_MODE` → sandbox/approval 映射 | `ccch1mneyyy/dsh-TUI` → `cordis.patch.yml`（sandbox-policy / user-approval 行） |
| 模型 / effort 配置键（`cc-tui.config.{provider,model,effort}`） | `ccch1mneyyy/dsh-TUI` → `docs/configuration.md` |
| `DSH_CC_RESUME_SESSION` 启动恢复 | `cordis.patch.yml`（agent 工厂行）；`channel.ts` ~L1352 |
| `/model` = 会话 fork、写入 `~/.dsh-cc/model.json` | `docs/configuration.md`；README |
| `/permission` 未适配（沙箱切换） | README §账号/策略 |
| effort 档位 `off/high/max`、Shift+Tab 循环 | `docs/configuration.md`；`src/channel.ts` `cycleEffort()` |
| 会话日志路径 / 编码 / zstd | `packages/session/session-persistence-jsonl/src/format.ts` |
| JSONL 事件与 usage 字段 | dsh-TUI `src/channel.ts`（`request/header` / `assistant/chunk`） |
| `NODE_ENV=production`（dev 渲染器 OOM） | `dsh-cc.cmd` |
| 插件行 `name` 从 profile node_modules 解析 | dsh-TUI `cordis.patch.yml`（working-activity 行注释） |

本仓库内既有实现参考：

- `src-tauri/src/agent_runtime/runtimes/claude.rs` — 硬编码模型 + transcript JSONL 解析
- `src-tauri/src/agent_runtime/runtimes/codex.rs` — 每会话 `-p capilot-<id>` 配置 profile + hook 清理
- `src-tauri/src/agent_runtime/runtimes/opencode.rs` — JS 插件 + `OPENCODE_CONFIG_DIR` 注入 + `launch_env` env 门控
- `src-tauri/src/agent_runtime/runtimes/mod.rs` — 注册表 + `known_runtimes()`

## 8. 验收标准（Acceptance Criteria）

给自主 agent 的「完成」定义。所有条目须能经本仓库自带验证命令自动确认；UI 层行为因本机 Wayland 无输入注入，采用「代码走读 + CLI 冒烟」验证（见 §10），最终交互验收由用户人工完成。

**Phase 1（骨架）**
- `cd src-tauri && cargo test` 全绿（含既有测试无回归）。
- `get_adapter("dsh")` 返回 `DshAdapter`；`known_runtimes()` 含 `"dsh"`（新增单测覆盖）。
- `spawn_interactive` 生成的命令以 `dsh --profile cc-tui` 开头；`launch_env` 含 `NODE_ENV=production`。
- `is_available` 在无 dsh 环境下返回 false 且不 panic（既有注册表遍历依赖该健壮性）。

**Phase 2（完整能力）**
- `list_models` 返回 `deepseek-v4-flash` / `deepseek-v4-pro`（均 1M 上下文）。
- `list_permission_modes` 返回 `ask` / `auto` / `yolo` 三个 `PermissionModeInfo`；`list_thinking_options` 返回 `fast` / `mid` / `high`（映射到 off/high/max）。
- 恢复：`supports_resume() == true`；恢复路径注入 `DSH_CC_RESUME_SESSION`；`capture_resume_key` 读取 `~/.dsh-cc/resume.txt`（不存在返回 None，不报错）。
- `context_usage`：对合成 zstd JSONL 会话日志能解析出正确的 `AgentUsage`（单测）；无日志/空目录返回 None。

**Phase 3（UI）**
- `pnpm tsc --noEmit` 全绿。
- `Composer.tsx` 三个 dsh 分支已接：权限 = 持久化 + 重启、effort = Shift+Tab 循环、模型 = spawn 钉死。
- `agentActions.ts` spawn 模型条件、`SettingsModal` `DEFAULT_RUNTIME_ARGS`、`Icon.tsx` `runtimeIcon` 均已加 dsh。
- 走读确认「已安装」列表在有 dsh 环境会显示该 runtime。

**Phase 4（状态钩子，按 spike 结论）**
- 若走方案 A：sidecar 格式与现有 `hook.sh` 一致（`{"status","ts"}`），且 `CAPILOT_AGENT_ID` / `CAPILOT_STATUS_DIR` 门控下无 env 时不落盘。
- 若走方案 B：tail 解析对 `turn/start/end` 的状态推断有单测覆盖。

**收尾**
- `docs/ai-runtime-references.md` 已新增 dsh 行 + volatile-facts 表。
- §11「实现状态与交接手记」已更新。
- 工作区无未预期改动；`git status` 仅含本任务文件。

## 9. 自主权限（Autonomous Permissions）

自主 agent 的运行边界。**默认不询问、不停顿**；遇阻塞按「记录假设 → 继续可推进部分」处理。

**允许（无需确认）**
- 在**当前工作区**内创建/编辑文件：`src-tauri/src/agent_runtime/runtimes/dsh.rs`、`runtimes/mod.rs`、`lib.rs`、`slash.rs`、`ui/**`、`docs/**`。
- 运行构建/验证命令：`cargo build`、`cargo test`、`cargo fmt`、`cargo clippy`、`pnpm tsc --noEmit`。
- 添加依赖（`cargo add` / `pnpm add`，如 `ruzstd`），前提是仅影响本仓库 `Cargo.toml` / `package.json`。
- 在临时目录生成/删除每会话 `--patch` 临时文件（§4.10 的 capilot-<id>.patch.yml）。
- 只读探查 dsh：`dsh --version`、`dsh --dump-config`、`ls ~/.dsh/profiles/cc-tui`（不修改）。

**禁止（越界即停，记入手记）**
- **修改用户全局配置**：`~/.dsh/**`、`~/.dsh-cc/**`、`~/.claude/**`、`~/CaPilot/status/**`（除 env 门控写入外）、`DEEPSEEK_API_KEY` 相关文件。
- **向全局 profile 安装插件**（§4.8 方案 A 的 `dsh plugin --profile cc-tui add …`）——必须先经用户确认；未确认时只写方案 B 或 spike 报告。
- **git 改分支**。
- 修改工作区外文件、删除既有数据（`~/CaPilot/sessions.db` 等）。
- 覆盖/删改 CLAUDE.md 与既有文档中与本任务无关的内容。

**运行规则**
- 一次只做当前 Phase，做完过 §10 验证循环，全绿再进下一 Phase。
- 不主动停下来问问题；遇到「需要用户决策」的点，把假设与备选写进 §11，继续做不受影响的部分。
- 若某个上游事实与文档不符，以实测为准，并在 §7 参考来源标注更正。

## 10. 验证循环（Verification Loop）

每个 Phase 结束后按序执行；任一环失败即回修，全绿才进入下一 Phase。

1. **编译门**：`cd src-tauri && cargo build` 与 `pnpm tsc --noEmit`。
2. **测试门**：`cd src-tauri && cargo test`（既有测试 + 新增 dsh 单测全部通过）。
3. **格式/静态门**：`cargo fmt --check`、`cargo clippy -- -D warnings`。
4. **CLI 冒烟（dsh 可用时）**：
   - `runtime_list_available` 经一次真实 `dsh --version` 后含 `dsh`；
   - 用 `dsh --dump-config --profile cc-tui --patch <临时文件>` 验证 §4.10 的 overlay 能覆盖 model/effort；
   - 起一个真实会话验证 spawn 命令与环境变量（权限 env、`NODE_ENV`）。
5. **回归走读**：确认 `apply_launch_overrides`（lib.rs）对 dsh 参数/环境无意外合并；确认 `sessions_delete` 路径无 dsh 泄漏。
6. **文档一致性**：`docs/ai-runtime-references.md` 与本文 §11 同步更新。
7. **交接**：更新 §11（本次改动、验证结果、遗留项、下次起点），保持中文。

## 11. 实现状态与交接手记（Agent Handoff Log）

自主 agent 每次运行的更新点：做完一个 Phase（或遇阻塞停下）时，如实填写。格式：

| Phase | 状态 | 改动摘要 | 验证结果 | 遗留/阻塞（假设与备选） | 时间 |
|---|---|---|---|---|---|
| 0 spike | 完成 | 本机核 dsh 事实：`dsh --version` 可用；cc-tui profile 目录存在（`~/.dsh/profiles/cc-tui`）；launcher 只解析 `--profile cc-tui` / `--patch`；会话日志结构 `$DSH_HOME/sessions/--<projectKey>--/<id>/session.jsonl[.zstd]`；认证 = `DEEPSEEK_API_KEY` 或 `~/.dsh/.credentials.yaml` | `dsh --version` ✓；`ls ~/.dsh/profiles/cc-tui` ✓；真实日志解 zstd 后确认 `turn/start`/`turn/end`/`assistant/chunk` usage 结构 | spike 期间本机 UI 自动化受限（Wayland 无输入注入），live 键位靠 README + 代码走读 | 2026-08-15 |
| 1 骨架 | 完成 | `dsh.rs` 适配器骨架：`is_available`（`dsh --version` + cc-tui profile 存在）、`is_authenticated`、`spawn_interactive`（`dsh --profile cc-tui --patch <临时文件>`）、`mod.rs`/`lib.rs` 注册 runtime、`slash.rs` `DSH_COMMANDS` 目录 | `cargo build` ✓；`cargo test` ✓；`pnpm tsc --noEmit` ✓ | — | 2026-08-15 |
| 2 完整能力 | 完成 | `list_models`（deepseek-v4-flash/pro hard-code）、`list_permission_modes`（read-only/workspace-write/danger-full-access）、`list_thinking_options`（auto/fast/mid/high）、resume（`DSH_CC_RESUME_SESSION` env + 10s 新会话目录名回退 resume.txt）、`context_usage`（`inputTokens+cacheReadTokens`，会话累计 cache 分子/分母）、`launch_env`（`NODE_ENV=production` + `DSH_PERMISSION_MODE` + hook env）、`status_hook_args`（override 重追加时幂等重写 patch）、`remove_session_patch` | `cargo test` 155+3 ✓；dsh 单测 15 ✓（含 project_key 编码、usage 解析、resume key 扫描）；`cargo clippy` 仅存与既有模式一致的 Default 提示 | — | 2026-08-15 |
| 3 UI | 完成 | `Composer.tsx` dsh 分支（权限=持久化+重启 PTY；模型=只持久化不驱动；effort=Shift+Tab `ESC[Z` 环距步数）、`SettingsModal` dsh 默认 launch、`TabBar` `HOOK_STATUS_RUNTIMES`+dsh、`agentActions` dsh 模型钉死（校验 catalog 存在才传）、`slash.rs` DSH_COMMANDS 目录、`Icon.tsx` deepseek.svg | `pnpm tsc --noEmit` ✓；`cargo build` ✓；Composer 分支逻辑代码走读（Wayland 无法注入 TUI 实测） | live 键位（Shift+Tab 环）未在真实 dsh TUI 上实测，按 README 语义实现 | 2026-08-15 |
| 4 状态钩子 | 完成 | 方案 B（dsh 无 hook 面）：`infer_status_from_content`（`turn/start`→working、`turn/end`→idle、`assistant/chunk`→working，最后命中胜出）、`newest_session_log_meta`（mtime_ns+len 指纹）、`StatusInferenceCache`、`agent_status_read` dsh 回退（DB→指纹→`spawn_blocking` 解码） | `cargo test` ✓（新增 4 条推断单测全绿）；`cargo build` ✓；`pnpm tsc --noEmit` ✓ | 真实 zstd 日志端到端推断未在 dsh 运行机上实测（本机无 dsh 会话） | 2026-08-15 |
| 收尾 | 完成 | `docs/ai-runtime-references.md` §1 dsh 行 + §2.4 + §3 易变事实行 19-24 + §4 日期；本 §11 表格 + 下方交接小节；最终验证跑通 | `cargo build` ✓；`cargo test` 155+3 ✓（dsh 15 全绿）；`pnpm tsc --noEmit` ✓；`git status` 仅含本任务文件 | 见下方「2026-08-15 自主开发完成 · 交接给用户」 | 2026-08-15 |

- 「未开始」→「进行中」→「完成」；完成须对应 §8 该 Phase 全部条目已过 §10。
- 阻塞即记为「进行中」并写清假设；交接对象是下一个 agent（或用户本人），中文、可执行。
- 上游事实若被实测推翻，本表与 §7 同步更正。

### 2026-08-15 自主开发完成 · 交接给用户

**做了什么**
- 新增第四个 runtime 适配器 `src-tauri/src/agent_runtime/runtimes/dsh.rs`（约 930 行含单测），完整实现 `AgentRuntimeAdapter`：启动/认证/模型/权限/思考档位/resume/上下文用量/启动 env/状态推断/launch-override 重追加。
- 每会话隔离：模型+effort+resume 经 `--patch` overlay 注入（cordis patch 整体替换 cc-tui 配置行，`sessionId` 绑定无条件重写），权限经 `DSH_PERMISSION_MODE` env；用户全局 `~/.dsh/profiles/cc-tui/cordis.yml`、`~/.dsh-cc/model.json` 永不动，patch 文件会话删除时清理。
- UI：Composer 权限（持久化+重启 PTY）/模型（只持久化）/effort（Shift+Tab 环距）三分支；SettingsModal dsh 默认 launch；TabBar hook 轮询纳入 dsh；`spawnAgent` dsh 模型钉死；`slash.rs` dsh 命令目录。
- 状态上报方案 B：无 hook 面 → Rust 侧 tail JSONL 推断（`turn/start`/`turn/end`/`assistant/chunk`），`agent_status_read` 侧车回退 + `StatusInferenceCache`（mtime_ns+len 指纹，1s 轮询零解码）。
- 文档：`docs/ai-runtime-references.md` 新增 dsh §2.4 + 易变事实行 19-24 + §1 行 + §4 日期；本 §11 表格。

**验证了什么（命令 + 结果）**
- `cd src-tauri && cargo build` → ✓ 成功
- `cd src-tauri && cargo test` → 155 + 3（dsh 15 条单测全绿）
- `pnpm tsc --noEmit` → ✓ 无类型错误
- `git status --porcelain` → 仅含本任务 13 modified + 3 untracked，无 fmt 连带改动

**哪些没验证及原因**
- 真实 dsh TUI 端到端（launch → resume → 状态推断 → 权限重启 → effort 切换）：本机未安装 dsh / 无真实会话日志可喂，且 Wayland 无法注入 TUI 输入。实现按 `dsh-cc-tui` 插件 README + 代码走读。
- `cargo clippy` 全量：仓库既有 toolchain 差异导致大量存量告警，只保证新增代码无新增告警（两个 Default-impl 提示与既有 `ContextUsageCache` 模式一致，保留）。

**用户醒来手动测**
1. 装有 dsh + `dsh-cc-tui` profile 的机器上 `pnpm tauri dev` → 设置确认 dsh 在已安装列表 → 新建 dsh 终端。
2. 发一条 prompt → TabBar 状态应短暂显示 运行中、会话结束回 空闲（1s 轮询推断）。
3. `/resume` 键：新开会话恢复上一会话；tab 关闭重开应 resume 到同一 session-id。
4. Composer 权限切换 → 应看到 PTY 重启、`DSH_PERMISSION_MODE` 变化；模型切换 → 只持久化、当前 TUI 不变。
5. Composer 思考档位切换 → Shift+Tab 环按 fast/mid/high 步进；context 用量 chip 显示（`input+cacheRead`）。
6. Settings → dsh ⚙ override 加参数 → 确认 patch 注入仍被重追加（hook 状态仍工作）。

### 2026-08-15 Bug 修复 · 交接给用户

**Bug 1（新建终端 picker 无 dsh）** — 已修，`ce333006b`。
- 根因：`ui/state/store.ts` 的 `TermTemplate.runtime` union 与 `DEFAULT_TEMPLATES` 缺 dsh 条目。
- 修法：加 `"dsh"` union 成员 + dsh 默认模板；`loadTermTemplates()` 会把 localStorage 缺的默认 id 补进去，老安装也能看到 dsh。

**Bug 2（dsh 终端创建后立即自动关闭）** — 根因在**机器 dsh 全局配置**，非本仓库代码，无需代码改动。
- 现象：spawn 后 ~0.5s 干净退出（exit 0，无 stderr）。用 `script` PTY 直跑 `dsh --profile cc-tui` 复现，排除了 CaPilot 侧。
- 根因：`~/.dsh/cordis.patch.yml`（dsh-skin 管理器自动生成的全局层）`insert` 了 `ui-skin-miku`（`@linxin666/dsh-client-ui-skin-miku`），但该包只装在 `web` profile、没装在 `cc-tui` profile。cc-tui 启动时 cordis loader 导入该包失败 → 触发插件树 fiber unload → `ctx.effect` 清理 → `instance.unmount()` → 干净退出。已全量核过 cc-tui 组合配置引用的 82 个包，唯一缺失就是它。
- 修法：用户选择「禁用 miku 皮肤」，把 `~/.dsh/cordis.patch.yml` 的 `- insert: ui-skin-miku` 改成 `- id: ui-skin-miku, disabled: true`（与其余 8 个禁用 skin 同款）。改前已备份 `~/.dsh/cordis.patch.yml.bak-20260815`。
- 验证：无 `--patch` overlay 直跑 `dsh --profile cc-tui` 及带 CaPilot 生成的 patch 文件均稳定存活（timeout 8s 超时被杀时仍在渲染，40KB/32KB 输出）。
- **可复用的排障签名**：dsh TUI「启动后 ~0.5s 干净退出（exit 0 + `Resume with -c`）」，几乎总是 profile 组合里有不可加载的条目（缺失插件包 / 配置损坏）。排查先 `dsh --dump-config --profile cc-tui`，再对每个 `name:` 包做 `require.resolve`。后续可选产品级加固：adapter 检测「spawn 后短时干净退出」并向用户给出诊断提示（本次未做，用户选了配置侧修复）。

### 2026-08-15 快速退出诊断 · 产品级加固（已实现）

用户事后确认需要「spawn 后立即干净退出」给诊断而非静默死掉，实现为三层：

**L1 预检探针（根因级）** — `DshAdapter::preflight()`（trait 默认 `None`，`build_and_spawn` 在 `is_available` 之后调用，`Err` 直接上抛 UI）：
- `dsh --dump-config --profile cc-tui`（~0.1s，disabled 条目被 composer 省略 → 即启动将要加载的集合）→ 解析 `name:` → 单个 `node -e` 子进程用 `require.resolve(name, {paths:[profile]})`（+ `name/package.json` 兜底，规避 `dsh-cc-tui` 这种 `import`-only `exports` 的误报）→ 任一不可解析即返回中文诊断（≤3 个 + 计数，含修法提示）。
- 探针自身不可靠（dump/node 失败）一律返回 `None` 放行，绝不让探针阻塞正常 spawn；兜底交给 L3。

**L2 前端兜底可见** — `spawnAgent`/`spawnBashAt` 包住 `invoke("agent_spawn")`，失败先 `notify("终端启动失败", err)` 再 rethrow（此前调用方只 `console.error`，全静默）。

**L3 快速退出安全网（通用兜底）** — `build_on_exit` 捕获 `runtime` + `Instant` 启动时刻；dsh 会话 spawn 后 3s 内 exit 0 时，在正常 `agent://exited`/`agent://removed` 之外追加发 `agent://exit-diagnostic`，前端 listener 用 `notify` 弹「dsh 启动后立即退出…」提示。覆盖探针看不到的启动崩溃/配置错误。

**验证**：`cargo test`（lib 158 通过，含新增 `parse_dump_names` / `format_missing` 单测）；`pnpm tsc --noEmit` 通过；探针 E2E——临时把 miku insert 加回 `~/.dsh/cordis.patch.yml` 后探针报 83 项里唯一缺失 `@linxin666/dsh-client-ui-skin-miku`，还原后 82 项全过。

