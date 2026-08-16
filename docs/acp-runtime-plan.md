# ACP Runtime 接入方案

> **日期:** 2026-08-17  
> **状态:** 设计稿（未实现）  
> **目标:** 用 [Agent Client Protocol](https://github.com/agentclientprotocol/agent-client-protocol) 作为通用通道，让 CaPilot 以**配置驱动**的方式接入新的 coding agent，避免再为每个 CLI 破解 TUI 方言。  
> **验证锚点（golden path）:** 本机 **OpenCode**（`opencode acp`）——用它跑通 Host + 面板 + Composer，并做**功能 / 显示 / 输入区**全量验收。  
> **非目标:** 把现有 claude / codex / dsh / pi 的 PTY 适配器迁到 ACP；也不把历史 `runtime: "opencode"` PTY 会话自动改成 ACP。

相关文档：

- 现有 PTY 集成事实：[`ai-runtime-references.md`](./ai-runtime-references.md)（含 OpenCode TUI 方言，作对照，**不是** ACP 路径）
- 安全约束：[`security-review.md`](./security-review.md)
- **无人值守多 agent 编排：** [`acp-multi-agent-dev-plan.md`](./acp-multi-agent-dev-plan.md)（状态：[`acp-dev-status.md`](./acp-dev-status.md)）
- OpenCode CLI：https://opencode.ai/docs/cli/（子命令 `opencode acp`）
- 协议站点：https://agentclientprotocol.com
- Rust SDK：`agent-client-protocol`（crates.io，文档见 docs.rs）
- 客户端参考（Zed external agents）：https://zed.dev/docs/ai/external-agents

---

## 1. 背景与问题

### 1.1 现状

CaPilot 的 agent 集成是 **PTY-first TUI 宿主**：

```
Composer / XTermPanel
        │ agent_write（文本 / 按键序列）
        ▼
   PtyBridge + portable-pty
        │ 原始 stdin/stdout 字节
        ▼
   claude | codex | dsh | pi | …（各自完整 TUI）
```

每个 runtime 实现 `AgentRuntimeAdapter`（`src-tauri/src/agent_runtime/adapter.rs`），但真正的成本在 TUI 方言：

| 成本项 | 落点 |
| --- | --- |
| Launch argv / env / 私有 config | `runtimes/<id>.rs` |
| Resume 发现（jsonl / sqlite / 目录编码） | 同上 |
| Status hooks | `status_hooks.rs` + 各 adapter 注入 |
| Composer live 控制（权限环、换模型、effort） | `ui/components/layout/Composer.tsx` 大量 `runtime ===` |
| Model catalog / usage | 各 adapter + `usage.rs` |

边际成本：每加一个「有自己 TUI」的 agent ≈ 再写 500–1500 行 Rust + 前端特判。

### 1.2 ACP 提供什么

ACP 把编辑器（Client）和 coding agent（Agent）之间的交互标准化为 **stdio 上的 newline-delimited JSON-RPC 2.0**：

```
Client spawn(command, args, env, cwd)
  → initialize { protocolVersion, clientCapabilities, clientInfo }
  ← { protocolVersion, agentCapabilities, authMethods, agentInfo }
  → authenticate?（若需要）
  → session/new | session/load
  → session/prompt { sessionId, prompt: ContentBlock[] }
      ← session/update*   （消息块 / tool_call / plan / usage…）
      ↔ session/request_permission
      ↔ fs/read_text_file | fs/write_text_file
      ↔ terminal/*
  ← { stopReason }
  → session/cancel（可中断）
```

传输要点（[protocol/transports](https://agentclientprotocol.com/protocol/transports)）：

- Agent 读 stdin、写 stdout；**stdout 只能是合法 ACP 消息**
- 消息以 `\n` 分隔，消息体内不得含裸换行
- stderr 可作日志
- Client 负责进程生命周期（spawn → 通信 → 关 stdin / kill）

**关键差异：** UI 归属从「agent 画 TUI」变为「client 画 UI，agent 只吐结构化事件」。因此 ACP 不能塞进现有「返回 `(cmd,args)` → PTY」路径，必须是**第二条 transport**。

### 1.3 设计原则

1. **双轨并存**：`transport = pty | acp`。现有主力 runtime 保持 PTY。
2. **配置驱动加 agent**：新 ACP agent = 一条 descriptor（command/args/env），不写新的 `runtimes/xxx.rs`。
3. **能力驱动 UI**：Composer / 面板只认 `transport` + `agentCapabilities`，禁止再堆 `runtime === "gemini"` / `runtime === "opencode"` 式 ACP 特判。
4. **不碰用户全局 CLI 配置**：与 `CLAUDE.md` 一致——仅 per-session env/argv；不建 managed agent home。OpenCode ACP **不要**注入现有 PTY 用的 `OPENCODE_TUI_CONFIG` / status 插件目录。
5. **高权限面收敛**：ACP 的 `fs/*`、`terminal/*`、permission 与现有 `agent_write` 同级，必须走沙箱与用户授权。
6. **以 OpenCode 验收通用层**：实现按通用 ACP 写；**门禁以 `acp:opencode` 的 E2E 清单为准**（§12）。过门后再加第二个 descriptor 证明边际成本。

---

## 2. 目标架构

```
                 ┌──────────────────────────────────────────┐
                 │              CaPilot IDE                   │
                 │  TabBar · Composer · 权限卡 · 文件/Git    │
                 └───────────────────┬──────────────────────┘
                                     │
                ┌────────────────────┼────────────────────┐
                │ transport: "pty"   │  transport: "acp"  │
                ▼                    ▼                    │
          PtyBridge             AcpBridge                 │
          (portable-pty)        (tokio process + NDJSON   │
                                + agent-client-protocol)  │
                │                    │                    │
                ▼                    ▼                    │
     claude/codex/dsh/pi      acp:opencode  ← 验证锚点     │
     （+ 历史 opencode PTY）   （+ 后续 gemini/goose/…）   │
     原生 TUI → xterm         ACP → AcpSessionPanel       │
```

### 2.1 Runtime 标识约定

| 类型 | `runtime` 字段示例 | `transport` |
| --- | --- | --- |
| 内置 PTY | `claude`, `codex`, `dsh`, `pi`, `bash-rc` | `pty`（可省略，默认） |
| 历史 PTY OpenCode | `opencode`（仅 resume 老会话；`known_runtimes` 已不提供新建） | `pty` |
| **验证用 ACP OpenCode** | **`acp:opencode`** | **`acp`** |
| 其他用户/内置 ACP | `acp:gemini`, `acp:goose`, `acp:custom-foo` | `acp` |

- 持久化主键仍用现有 `runtime: String`（`AgentSessionRecord` / `AgentMeta`）。
- **不强制改 SQLite schema**：`runtime` 以 `acp:` 前缀区分即可；前端/后端用 `runtime.startsWith("acp:")` 或集中函数 `is_acp_runtime(id)` 判断。
- **禁止**把 `acp:opencode` 与 PTY `opencode` 混为同一 id：Composer / ContentArea 里大量 `runtime === "opencode"` 是 **PTY 按键方言**，ACP 会话若撞上会误走 F12/Ctrl+T/`agent_write`。
- 若后续需要显式列，再加可空 `transport TEXT`（迁移可选，非 MVP）。

`resume_key` 语义复用：

- PTY：各 provider 自己的 session id  
- ACP：ACP `sessionId`（`session/new` 或 `session/load` 返回值）——与 OpenCode 内部 session id 是否相同以 Phase 0 实测为准，CaPilot 只当不透明字符串持久化

### 2.2 Descriptor（ACP agent 描述符）

路径（优先级从高到低）：

1. 用户配置：`~/CaPilot/acp-agents.json`
2. 应用内置推荐列表：`src-tauri/resources/acp-agents.default.json`  
   - **MVP 必带** `opencode` 一条，保证干净环境开箱可验

Schema（**验证锚点写死为第一条**）：

```jsonc
{
  "version": 1,
  "agents": [
    {
      "id": "opencode",                  // → runtime id = "acp:opencode"
      "name": "OpenCode (ACP)",
      "command": "opencode",
      "args": ["acp"],                   // 本机 1.18.18：`opencode acp` = ACP stdio server
      "env": {},                         // 勿注入 OPENCODE_TUI_CONFIG / status plugin
      "cwd_mode": "session",             // Host 在 spawn 时设 process cwd = AgentSession.cwd
                                         // 亦可附加 args "--cwd", "<session.cwd>"（Phase 0 二选一钉死）
      "icon": null,
      "enabled": true
    }
    // 验收通过后再加：
    // { "id": "gemini", "command": "gemini", "args": ["…"], ... }
  ]
}
```

对标 Zed 的 `agent_servers` custom agent（`command` + `args` + `env`）。  
**加新 agent 的用户路径：** Settings → 添加 ACP Agent → 填 command/args → 保存 → 出现在 runtime 列表。

发现 `available`：MVP 用 `which(command)`。`authenticated`：MVP 对 OpenCode 可探测既有 provider 凭证（可选）；失败留到 `initialize`/`prompt` 时表面错误（不代管 `opencode auth`）。

### 2.3 为何选 OpenCode 做验证锚点

| 理由 | 说明 |
| --- | --- |
| 本机已装且一等公民 | `opencode acp` 子命令官方存在（CLI help + [docs](https://opencode.ai/docs/cli/)），stdio NDJSON |
| 有 PTY 对照基线 | 仓库已有完整 `runtimes/opencode.rs` + Composer 方言，便于对照「ACP 不该出现的 UI」 |
| 不污染主力新建列表 | PTY `opencode` 已不在 `known_runtimes()`；`acp:opencode` 是**新**入口，不复活 TUI 新建路径 |
| 权限/工具真实 | 编码 agent 会触发 read/edit/bash 类 tool + permission，够验 Host 与面板，不靠纯 echo mock 过门 |
| 复用用户已有登录 | 一般已配置 provider；减少 Phase 0 卡在 auth |

**明确不是：** 把 OpenCode 的 TUI 功能（F12 命令面板、Ctrl+T variant、Build/Plan Tab、status 插件）搬到 ACP。那些仍只属于 `runtime === "opencode"` 的 PTY 会话。

---

## 3. 后端设计

### 3.1 目录与模块

```
src-tauri/src/agent_runtime/
├── adapter.rs          # 现有 PTY trait（基本不动）
├── pty_core.rs         # 不动
├── status_hooks.rs     # ACP 不用
├── runtimes/           # 现有 PTY adapters
└── acp/                # 新增
    ├── mod.rs          # 模块导出、is_acp_runtime、runtime id 工具
    ├── descriptor.rs   # 读写 ~/CaPilot/acp-agents.json
    ├── registry.rs     # 合并进 runtime_list_available
    ├── bridge.rs       # AcpBridge：进程表、与 lib.rs 对称的 API
    ├── host.rs         # 单会话：spawn、JSON-RPC 循环、Client 回调
    ├── events.rs       # 推前端的事件 DTO
    └── permission.rs   # request_permission ↔ 前端应答 状态机
```

`agent_runtime/mod.rs` 增加 `pub mod acp;`。

### 3.2 依赖

`src-tauri/Cargo.toml`：

```toml
agent-client-protocol = "2"   # 以 crates.io 当时稳定版为准；实现时锁定 minor
```

优先使用官方 crate 的 `Client` / `Stdio` / `AcpAgent` 路径（见 docs.rs quick start），避免手写 JSON-RPC 分帧——除非 crate API 与 Tauri 长生命周期会话模型严重不合，再降级为自研 NDJSON 循环（仍用 schema 类型）。

异步运行时：项目已有 `tokio`（process / io-util / sync / rt-multi-thread）。ACP 进程用 **普通管道**，**不要** `portable-pty`。

### 3.3 `AcpBridge` API（与 `PtyBridge` 平行）

```rust
// 概念接口（实现时可调整命名）
pub struct AcpBridge { /* sessions: Mutex<HashMap<AgentId, AcpSessionHandle>> */ }

impl AcpBridge {
    pub fn start(&self, id, desc, session, event_sink) -> Result<AgentInfo, …>;
    pub fn prompt(&self, id, blocks: Vec<ContentBlock>) -> Result<(), …>;
    pub fn cancel(&self, id) -> Result<(), …>;
    pub fn respond_permission(&self, id, request_id, outcome) -> Result<(), …>;
    pub fn kill(&self, id) -> Result<(), …>;
    pub fn status(&self, id) -> Option<AcpStatus>;
    // resize：no-op（无 PTY）
}
```

`AcpSessionHandle` 内部持有：

- child process（stdin/stdout）
- ACP `sessionId`（写入 persistence `resume_key`）
- 协商到的 `agentCapabilities`
- 当前 turn 状态、pending permission oneshot
- 可选：stderr 环形缓冲（调试）

### 3.4 会话状态机

```
           start()
             │
             ▼
        Connecting ──initialize fail──► Failed
             │
             ▼
     Authenticating? ──fail──► Failed
             │
             ▼
        Ready/Idle
             │ prompt()
             ▼
         Running ──session/update*──►（前端渲染）
             │
             ├─ request_permission ──► WaitingPermission ──respond──► Running
             ├─ cancel ──────────────► Cancelling ──► Idle (stopReason=cancelled)
             ├─ stopReason ──────────► Idle
             └─ process exit ────────► Done / Failed
```

映射到现有 `AgentStatus` / 前端 tab 文案：

| ACP | CaPilot status / hook 语义 |
| --- | --- |
| Connecting / Authenticating | `running` |
| prompt 进行中 | `running`（UI「运行中」） |
| `request_permission` | `waiting_input` |
| elicitation（若实现） | `awaiting_choice` |
| turn 结束 `end_turn` | `idle` |
| 进程退出 | `done` / `failed` |
| `usage_update{used,size}` | `AgentUsage.context_window_used_tokens/max` |

**不需要** `status_hooks.rs`：状态由协议事件直接驱动，写内存 + 可选侧车/DB。

### 3.5 Client 能力（initialize 时声明）

MVP：

```jsonc
{
  "fs": { "readTextFile": true, "writeTextFile": false },  // 写二期再开
  "terminal": false,
  "elicitation": null
}
```

二期再开：

- `fs.writeTextFile`（workspace 路径校验 + 可选用户确认）
- `terminal: true` → 内部复用 `PtyCore` 开无头 PTY，实现 `terminal/create|output|wait|kill|release`
- elicitation form/url

**路径规则：** 所有 fs 路径必须是绝对路径；解析后必须落在 `session.cwd` 的允许根之内（symlink 解析后比较），否则拒绝。对齐 `docs/security-review.md` 对高权限 IPC 的要求。

### 3.6 Permission 策略与 CaPilot `mode` 映射

| CaPilot mode | ACP `request_permission` 行为 |
| --- | --- |
| `ask` | 一律弹前端，等 `acp_respond_permission` |
| `auto` | 只读类 / 工作区内 edit 可自动 allow；destructive / 出界 deny 或升级询问（实现时给可配置表） |
| `yolo` | 全部 allow（仍记日志） |

MVP 可先：**只有 ask**（所有 permission 进 UI）；auto/yolo 二期。避免静默写盘。

### 3.7 与 `lib.rs` 接合

现有路径：

- `build_and_spawn` → `get_adapter` → `spawn_interactive` → `PtyBridge::spawn`
- `agent_write` / `agent_resize` / `agent_kill` / `agent_resume` / `runtime_list_available`

改动要点：

1. **`runtime_list_available`**  
   - 先推现有 `known_runtimes()`  
   - 再 `acp::registry::list()` 把每个 descriptor 收成 `RuntimeInfo`：  
     - `id = "acp:{desc.id}"`  
     - `models = []`（MVP；若后续 agent 暴露 configOptions 再填）  
     - `permission_modes` = CaPilot 通用 ask/auto/yolo 或仅 ask  
     - `thinking_options = []`  
     - `available = command_exists`  
     - 可加字段（见 §3.9）`transport: "acp"`

2. **`agent_spawn` / `agent_resume`**  
   ```text
   if is_acp_runtime(&runtime) {
       let desc = registry.get(strip_prefix(runtime))?;
       // resume: 若有 resume_key 且 cap.loadSession → session/load
       // 否则 session/new
       acp_bridge.start(...)
   } else {
       现有 build_and_spawn
   }
   ```

3. **`agent_write` / `agent_resize`**  
   - ACP id → `Err("ACP session does not accept PTY writes")` 或静默忽略（推荐 **明确报错**，便于前端发现误用）

4. **`agent_kill`**  
   - 两桥都试 / 按 runtime 分发：`acp_bridge.kill` 发 cancel + kill child

5. **新 Tauri commands**

   | Command | 参数 | 说明 |
   | --- | --- | --- |
   | `acp_prompt` | `{ id, text }` | 组装 `ContentBlock::Text`，`session/prompt` |
   | `acp_cancel` | `{ id }` | `session/cancel` |
   | `acp_respond_permission` | `{ id, requestId, outcome }` | allow / deny / cancelled |
   | `acp_list_agents` | — | 读 descriptor 列表（Settings） |
   | `acp_upsert_agent` | `descriptor` | 写 `acp-agents.json` |
   | `acp_remove_agent` | `{ id }` | 删除条目 |
   | event / Channel | `acp_event` | 见 §3.8 |

6. **AppState**  
   - `lib.rs` / managed state 增加 `Arc<AcpBridge>`，与 `PtyBridge` 并列初始化。

### 3.8 前端事件模型

推荐 **Tauri event**（全局 `acp://event`）或 per-session `Channel<AcpEvent>`（与 PTY 的 `Channel<Vec<u8>>` 对称）。

```ts
// 概念 DTO（camelCase，serde rename）
type AcpEvent =
  | { type: "session_started"; sessionId: string; capabilities: AgentCapabilities }
  | { type: "message_chunk"; messageId?: string; text: string }
  | { type: "tool_call"; toolCallId: string; title: string; kind?: string; status: string }
  | { type: "tool_call_update"; toolCallId: string; status: string; detail?: string }
  | { type: "plan"; entries: { content: string; priority?: string; status?: string }[] }
  | { type: "usage"; used: number; size: number }
  | { type: "permission_request"; requestId: string; toolCallId?: string; summary: string; raw?: unknown }
  | { type: "turn_done"; stopReason: string }
  | { type: "status"; status: "idle" | "running" | "waiting_input" | "failed" | "done" }
  | { type: "error"; message: string }
  | { type: "stderr"; line: string };  // 可选，调试
```

`session/update` 的 wire 变体（实现时以官方 schema 为准）至少处理：

- `agent_message_chunk`
- `tool_call` / `tool_call_update`
- `plan`
- `usage_update`

未识别的 update：**忽略并 log**，不要断会话。

### 3.9 `RuntimeInfo` 扩展（建议）

```rust
// adapter.rs RuntimeInfo
pub transport: String,  // "pty" | "acp"，默认 "pty" 保持兼容
// 可选：
// pub acp_command: Option<String>,  // Settings 展示用
```

前端 `RuntimeInfo` 同步加 `transport?: "pty" | "acp"`。  
旧前端忽略未知字段也安全（serde 默认）。

### 3.10 错误与预检

- descriptor 缺 command → 列表里 `available: false`
- spawn 失败 → `agent_spawn` 返回中文可读错误
- initialize 版本不兼容 → 杀进程 + 错误「协议版本不兼容」
- auth 需要交互而我们未实现 auth UI → 错误里带 stderr 尾部，提示用户先在终端完成该 CLI 的 login
- prompt 中进程崩溃 → `Failed` + 事件

---

## 4. 前端设计

### 4.1 核心分叉点

| 文件 | 改动 |
| --- | --- |
| `ui/state/store.ts` | `RuntimeInfo.transport`；agent 可挂 `acpCapabilities`；`acpEvents` / 消息缓冲（或独立 store slice） |
| `ui/state/agentActions.ts` | `sendPromptToAgent`：`isAcp(runtime) ? invoke("acp_prompt") : agent_write…`；spawn 后订阅事件 |
| `ui/components/layout/ContentArea.tsx` | `Panel`：`isAcpRuntime(runtime)` → `<AcpSessionPanel>`，否则 `<XTermPanel>`；**不得**用 `runtime === "opencode"` 判断 ACP（那是 PTY） |
| `ui/components/layout/Composer.tsx` | ACP 目标：隐藏全部 PTY 方言控件（含 OpenCode 的 F12 权限、Ctrl+T variant、Build/Plan、模型 TUI 注入）；发送走 `acp_prompt`；显示取消 → `acp_cancel`；见 §4.3 / §12.3 |
| `ui/components/acp/AcpSessionPanel.tsx` | **新建** 结构化会话 UI（OpenCode 验收主画面） |
| `ui/components/acp/AcpPermissionCard.tsx` | **新建** 权限确认 |
| `ui/components/layout/SettingsModal.tsx` | 「ACP Agents」：列表 / 添加 / 编辑 / 删除 descriptor |
| `ui/components/layout/TerminalTemplatePicker.tsx` | runtime 列表含 `acp:opencode`（来自 `runtime_list_available`） |
| `ui/components/layout/TabBar.tsx` | 状态点读统一 agent status；ACP **不要**依赖 hook 侧车轮询——`agent_status_read` 对 ACP 读 bridge（§5） |
| `ui/components/layout/ContentArea.tsx` 其它 | 现有 `openCodeTabs` / `runtime === "opencode"` 仅服务 **PTY** OpenCode（如特殊快捷键）；`acp:opencode` 不得进入这些分支 |

### 4.2 `AcpSessionPanel` MVP 结构

```
┌─ AcpSessionPanel ─────────────────────────────────────┐
│ （可选）Plan：☐ … ☑ …                                  │
│                                                       │
│  assistant: 流式 markdown / 纯文本                     │
│                                                       │
│  ┌ tool: Edit src/foo.rs · completed ───────────────┐ │
│  │ title + 简短 detail                               │ │
│  └───────────────────────────────────────────────────┘ │
│                                                       │
│  ⚠ 权限：Run bash "npm test"                          │
│     [允许] [拒绝]                                      │
│                                                       │
│  ── turn end (end_turn) ──                            │
│  stderr 折叠（调试）                                   │
└───────────────────────────────────────────────────────┘
```

MVP **不做**：完整 diff 审阅、嵌入式终端、图片/音频 content、完美 markdown 主题。  
文本先 `whitespace: pre-wrap`；后续再上 markdown 渲染。

### 4.3 Composer 行为矩阵（含 OpenCode 对照）

| 操作 | PTY（含历史 `opencode`） | ACP（`acp:opencode` 及任意 `acp:*`） |
| --- | --- | --- |
| 发送 | `agent_write`（+ codex 分写 CR） | **`acp_prompt` only**；禁止 `agent_write` |
| 取消 | 注入 Ctrl+C（若有） | `acp_cancel`；Composer 显式「停止」按钮（turn 进行中） |
| 权限模式 UI | OpenCode：F12/Ctrl+P + 「Enable/Disable auto-approve…」按键串 | **只**改 CaPilot 侧 mode 持久化 + Host 自动批准策略（§3.6）；**零** `agent_write` |
| 换模型 | OpenCode：命令面板 type `model` + 显示名 | MVP：**隐藏**模型菜单，或只读展示 `agentInfo`；**禁止** F12 注入路径 |
| 思考 / variant | OpenCode：Ctrl+T + ⚡ 按钮读 `model.json` | MVP：**隐藏** ⚡ 与 Ctrl+T 劫持 |
| Build / Plan | OpenCode：Tab 循环 agent 模式按钮 | MVP：**隐藏** |
| `/` slash | `slash.rs` 按 runtime（opencode 有自己的列表） | MVP：空或仅 CaPilot 本地命令；**不要**复用 PTY opencode slash 表 |
| 终端焦点 F1 | 聚焦 xterm | 无 PTY：聚焦消息列表或 no-op，**不要**对 ACP 调 resize 脉冲 |
| 多目标 Tab 循环 | 现有逻辑 | 仍可用；目标为 ACP 时发送走 `acp_prompt` |

**回归护栏（实现时写进 Composer 条件）：**

```ts
// 所有「OpenCode TUI 方言」分支必须同时满足非 ACP：
if (agent.runtime === "opencode" && !isAcpRuntime(agent.runtime)) { /* PTY only */ }
// 更干净：PTY 方言只认精确 id
if (agent.runtime === "opencode") { /* PTY — acp:opencode 进不来 */ }
if (isAcpRuntime(agent.runtime)) { /* 通用 ACP 控件集 */ }
```

因 `acp:opencode` ≠ `opencode`，现有 `=== "opencode"` 不会误匹配——**但** ContentArea 的 `openCodeTabs`、status hook 列表、`opencode_current_variant` 轮询等必须确认不会把 `acp:opencode` 当 PTY 处理（例如对 ACP tab 调 `opencode_current_variant` 或 resize 脉冲）。

抽公共 helper：

```ts
// ui/state/runtimeTransport.ts
export function isAcpRuntime(id: string | undefined): boolean {
  return !!id && id.startsWith("acp:");
}
export function runtimeTransport(id: string | undefined): "pty" | "acp" {
  return isAcpRuntime(id) ? "acp" : "pty";
}
```

### 4.4 订阅生命周期

1. `agent_spawn`（`acp:opencode`）返回后，前端 `listen("acp://event", …)` 或持有 Channel  
2. 按 `agentId` 过滤事件，append 到该 session 的消息列表；**自动滚到底**（用户上翻锁定除外）  
3. `agent_kill` / tab 关闭：取消 listen；后端 cancel + kill child  
4. 恢复会话：`agent_resume` → 若 `loadSession` 成功，面板可先空（无历史回放，MVP 可接受）；二期 history  
5. **切 tab / 分屏**：后台 ACP 会话事件仍要入 store，回到 tab 不丢 chunk（验证项 §12.2）

---

## 5. 持久化

| 数据 | 位置 | 说明 |
| --- | --- | --- |
| 会话行 | `sessions.db` | `runtime="acp:opencode"`, `resume_key=<acp sessionId>` |
| meta | `…/agents/<id>/.agent-meta.json` | 同现有字段，无需新键 |
| descriptor | `~/CaPilot/acp-agents.json` + 内置 default（含 opencode） | 用户可编辑 |
| ACP 消息历史 | MVP **不持久化** | 进程内 + 前端内存；重启只 resume agent 侧 session（若支持） |
| status 侧车 | **不使用** OpenCode PTY 的 status 插件 / hook 文件 | 状态在 `AcpBridge` 内存；`agent_status_read` 对 ACP 读 bridge |

`agent_status_read`：若 id 在 `AcpBridge` 中，返回 bridge 状态；否则走现有 hook / dsh 推断逻辑。  
TabBar 对 ACP 会话应能显示 运行中 / 待确认 / 空闲，**不**要求 `HOOKED_RUNTIMES` 包含 `acp:opencode`。

---

## 6. 分阶段交付（以 OpenCode 为门禁）

每阶段 **出口条件** 必须用 `acp:opencode`（或同 phase 的 mock）验证；§12 为跨 phase 总验收表，Phase 2+ 每完成一块就勾选对应行。

### Phase 0 — OpenCode ACP 协议摸底（0.5–1 天）

本机已确认：`opencode` 1.18.18，`opencode acp` =「start ACP server」，stdio NDJSON；`--cwd` 可用。`acp` 子命令 help **未**暴露 `--model` / `--session`（与 TUI 不同）——session/model 走协议内方法。

- [ ] 脚本或官方 Rust client：`spawn opencode acp` → `initialize` → 记录 `agentCapabilities` / `authMethods` / `agentInfo`
- [ ] `session/new`（cwd = 临时目录或本仓库）→ 记录 `sessionId`
- [ ] `session/prompt` 一句无工具问题（如「只回复 pong」）→ 收集 `session/update` 变体与 `stopReason`
- [ ] 再 prompt 一句会触发读文件/列目录的任务 → 观察 `tool_call*`、是否 `request_permission`、client `fs/*` 是否被调用
- [ ] 探测 `session/load` 是否在 capabilities 中；若有，用上一步 sessionId 冷启动再 load
- [ ] 探测 cancel：prompt 中发 `session/cancel` 的行为
- [ ] stderr 是否干净（不可把日志打进 stdout）
- [ ] 结论写入 **附录 A**（capabilities 原文摘要 + 样例日志路径）

**出口条件：** 附录 A 填完；至少一轮 OpenCode prompt 的原始 NDJSON 日志可复现。

### Phase 1 — 后端 Host MVP（OpenCode + mock）

- [ ] `Cargo.toml` 加依赖  
- [ ] `acp/descriptor.rs`：默认列表含 `opencode`；`which` 探测  
- [ ] `acp/host.rs` + `bridge.rs`：spawn `opencode acp`、initialize、session/new、prompt、update 转发  
- [ ] `acp/events.rs` + Tauri event  
- [ ] `runtime_list_available` 合并 → 出现 `acp:opencode` 且 `transport=acp`  
- [ ] `agent_spawn` / `agent_kill` 分叉；**`get_adapter` 前**拦截 `acp:*`  
- [ ] `acp_prompt` / `acp_cancel`  
- [ ] 测试 A：mock agent 固定 NDJSON（CI 稳定）  
- [ ] 测试 B（`#[ignore]` 或 feature）：本机 `opencode acp` 真连一轮 prompt（开发机门禁）

**出口条件：** mock 测试绿；开发机上 `acp_prompt` 对 OpenCode 能收到 ≥1 条 `message_chunk` 并 `turn_done`（可无 UI）。

### Phase 2 — 前端面板 + 输入区 MVP（OpenCode UI 门禁）

- [ ] `runtimeTransport.ts` + store 消息缓冲  
- [ ] `AcpSessionPanel` + `ContentArea` 按 `isAcpRuntime` 分叉  
- [ ] `sendPromptToAgent` → `acp_prompt`  
- [ ] Composer：§4.3 ACP 列控件集；**停止**按钮  
- [ ] Tab 状态：running / idle 随事件变化  
- [ ] **执行 §12.2 显示验收 + §12.3 输入区验收（OpenCode）**

**出口条件：** 用户只通过 UI：选 OpenCode (ACP) → 发消息 → 流式看见回复；Composer 无 PTY OpenCode 方言按钮误显；§12.2 / §12.3 清单全部勾选或注明「能力不支持」。

### Phase 3 — 权限与安全（OpenCode 触发真工具）

- [ ] `request_permission` → 卡片 → allow/deny  
- [ ] 用 OpenCode prompt 触发一次需确认的工具（编辑或 bash）  
- [ ] deny 后确认工具未继续；allow 后 `tool_call_update` completed  
- [ ] `fs/read_text_file` cwd 沙箱（若 OpenCode 走 client fs）  
- [ ] mode 策略；`security-review.md` 补丁  
- [ ] **执行 §12.1 功能项中的 permission / cancel / kill**

**出口条件：** §12.1 权限相关行通过；出界 read 拒绝。

### Phase 4 — 产品化

- [ ] Settings CRUD（改 args 后仍能起 OpenCode）  
- [ ] `session/load` + `resume_key`（若 Phase 0 证实 OpenCode 支持）  
- [ ] `usage_update` → 上下文条（若 agent 发）  
- [ ] stderr 折叠  
- [ ] `ai-runtime-references.md` 增加 § ACP / OpenCode ACP  
- [ ] **完整跑一遍 §12 总表 + PTY 回归（claude 或现有 cargo test）**

### Phase 5 — 增强（按需）

- [ ] `fs/write` / `terminal/*` / configOptions 模型 UI  
- [ ] 第二条第二个 agent descriptor 证明「只加配置」  
- [ ] 消息历史落盘  
- [ ] ACP Registry

---

## 7. 文件级改动清单（按 Phase）

### Phase 1

| 路径 | 动作 |
| --- | --- |
| `src-tauri/Cargo.toml` | 加 `agent-client-protocol` |
| `src-tauri/src/agent_runtime/mod.rs` | `pub mod acp` |
| `src-tauri/src/agent_runtime/acp/*.rs` | 新建 |
| `src-tauri/src/agent_runtime/adapter.rs` | `RuntimeInfo.transport` 可选字段 |
| `src-tauri/src/lib.rs` | 注册 `AcpBridge`、commands、spawn/kill/list 分叉 |
| `src-tauri/tests` 或 `acp/host.rs` `#[cfg(test)]` | mock agent 测试 |
| `docs/ai-runtime-references.md` | 链到本文 |

### Phase 2

| 路径 | 动作 |
| --- | --- |
| `ui/state/runtimeTransport.ts` | 新建 |
| `ui/state/store.ts` | transport、acp 消息状态 |
| `ui/state/agentActions.ts` | send/spawn 分发 |
| `ui/components/acp/AcpSessionPanel.tsx` | 新建 |
| `ui/components/layout/ContentArea.tsx` | Panel 分叉 |
| `ui/components/layout/Composer.tsx` | 控件显隐 + cancel |

### Phase 3–4

| 路径 | 动作 |
| --- | --- |
| `ui/components/acp/AcpPermissionCard.tsx` | 新建 |
| `ui/components/layout/SettingsModal.tsx` | CRUD UI |
| `src-tauri/src/agent_runtime/acp/permission.rs` | 策略 |
| `src-tauri/src/agent_runtime/acp/descriptor.rs` | 写回 json |
| `docs/security-review.md` | ACP 威胁面补丁 |
| `docs/CaPilot-IDE-RUNBOOK.md` | 用户如何加 ACP agent |

**明确不改（MVP）：**  
`pty_core.rs`、`status_hooks.rs`、各 `runtimes/claude|codex|dsh|pi.rs` 业务逻辑、**PTY** `runtimes/opencode.rs`（保留 resume 老会话）、现有 session 文件格式。  
**禁止**为 ACP 复用 OpenCode 的 `OPENCODE_TUI_CONFIG` / `OPENCODE_CONFIG_DIR` status 插件注入。

---

## 8. 测试计划

### 8.1 自动化

1. **Mock agent**（CI）：固定 `initialize` / `session/new` / `session/prompt` + chunk + 可选 permission  
2. **Rust**：`AcpBridge` 对 mock 的 prompt / cancel / permission 应答  
3. **沙箱**：`fs/read` 出界 error  
4. **回归**：现有 `cargo test` 全绿；`pnpm tsc --noEmit`  
5. **可选本机：** `#[ignore] opencode_acp_smoke` — 检测 `opencode` 在 PATH 时跑一轮真 prompt  

### 8.2 手工（主路径 = OpenCode ACP）

完整清单见 **§12**。最少路径：

1. Runtime 列表出现 **OpenCode (ACP)** / `acp:opencode` 且 available  
2. 新建会话 → **不是** xterm，而是 `AcpSessionPanel`  
3. Composer 发送 → 流式回复；无 PTY OpenCode 的 ⚡/Build 按钮  
4. 触发 permission → 拒绝 / 允许  
5. 取消 turn；关 tab kill 进程  
6. 并排开一个 **claude PTY** tab，确认行为与改前一致  

---

## 9. 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| 官方 crate API 偏 one-shot，难嵌 Tauri 长会话 | Phase 0 用 OpenCode 验证；不行则自研 NDJSON + schema 类型 |
| OpenCode `acp` 与 TUI 能力不一致（无 CLI `--model`） | 模型/模式 MVP 隐藏；不在 Composer 伪造 TUI 能力 |
| OpenCode 未登录 / 无 provider | Phase 0 记录错误形态；UI 展示 stderr / error 事件，引导 `opencode auth` |
| 与 PTY `opencode` id 混淆 | 固定 `acp:opencode`；验收 §12.3 反例项 |
| ContentArea `openCodeTabs` 等 PTY 特判误伤 | code review + §12.2 检查项 |
| Auth 要交互 TUI | 不代管；失败可读 |
| fs/terminal 攻击面 | 默认关 write/terminal；path 规范 |
| `get_adapter("_")` → Claude | `is_acp_runtime` 在前硬拦截 |
| Composer 误 `agent_write` | 后端硬拒 + 前端 `isAcpRuntime` 双保险 |
| 用户以为 ACP=完整 OpenCode TUI | 名称用「OpenCode (ACP)」；文档说明 IDE 渲染 |

---

## 10. 非目标（再强调）

- ❌ 用 ACP 重写 claude/codex/dsh/pi  
- ❌ 删除或迁移已有 PTY `opencode` 会话  
- ❌ 在 ACP 路径复刻 OpenCode TUI 方言（F12、Ctrl+T、Build/Plan 按键）  
- ❌ 为 ACP 建 managed agent home / 改用户全局 config  
- ❌ MVP 完整复刻 Zed Agent Panel  
- ❌ 在 xterm 里混用 ACP RPC  

---

## 11. 成功标准

1. **锚点闭环：** `acp:opencode` 在 UI 完成 spawn → prompt → 流式回复 → permission → cancel → kill，且 **§12 三张表**无未豁免失败项。  
2. **显示正确：** 结构化面板无错位/空白死局；Tab 状态与 turn 一致；不出现 PTY OpenCode 专属控件。  
3. **输入区正确：** Composer 发送/停止/权限策略走 ACP；无 `agent_write` 泄漏；多 tab 目标切换不串台。  
4. **双轨无损：** PTY `cargo test` 绿；手工 claude（或任意 PTY）会话无回归。  
5. **边际成本（延伸）：** 在 OpenCode 过门后，新增第二个已支持 ACP 的 agent ≤ 一条 descriptor + 冒烟。  
6. **安全：** 默认不写盘、出界读拒绝、permission 默认 ask。  

---

## 12. OpenCode 验收规范（功能 · 显示 · 输入区）

> **目的：** 「接入了」不等于「能用」。下列项是 Phase 2–4 的 **产品门禁**。  
> **环境：** 本机 `opencode` ≥ 1.18.x 在 PATH；已能 `opencode auth` / providers 可用；测试 cwd 建议用临时目录或本 worktree。  
> **runtime：** 仅 `acp:opencode`。  
> **记录：** 失败记「步骤 / 期望 / 实际 / 截图或日志」；能力缺失（如无 loadSession）标 **N/A（附录 A）** 不算失败。

### 12.1 功能是否真正实现

| # | 步骤 | 期望 | Phase |
| --- | --- | --- | --- |
| F1 | 打开新建终端/runtime 选择器 | 可见 **OpenCode (ACP)**（或等价文案），id 侧为 `acp:opencode`；`available=true`（opencode 在 PATH 时） | 1–2 |
| F2 | 选中并创建会话 | `agent_spawn` 成功；进程为 `opencode acp`（可用 `ps`）；**不**创建 PTY 字节 channel | 1–2 |
| F3 | Composer 输入 `只回复一个词：pong` 发送 | 收到流式或完整 assistant 文本，含 pong；出现 `turn_done` / 状态回 idle | 1–2 |
| F4 | 再发一轮跟进问题 | 第二轮仍可用（同 session）；不需重建 tab | 2 |
| F5 | 发送会触发工具的任务（如「读取当前目录下 README 的第一行并引用」） | 面板出现 `tool_call` / 更新；最终有基于工具结果的回复 | 2–3 |
| F6 | 在 ask 模式下触发需授权工具 | UI 出现权限卡；**未点允许前**工具不完成 | 3 |
| F7 | 权限卡点「拒绝」 | turn 安全结束或 agent 改策略；无静默执行；可继续新 prompt | 3 |
| F8 | 权限卡点「允许」 | `tool_call` → completed；回复继续 | 3 |
| F9 | turn 进行中点 Composer「停止」/ `acp_cancel` | 尽快 `stopReason=cancelled` 或等价；状态非卡死 running；可再发新 prompt | 2–3 |
| F10 | 关闭 tab / kill | 子进程退出；无僵尸 `opencode acp` | 2 |
| F11 | 重启 IDE，resume 该会话（若附录 A 支持 loadSession） | 进程起来且 `session/load`；至少能继续 prompt（历史 UI 可空） | 4 |
| F12 | F11 不支持时 | 明确失败提示或降级 `session/new`，**不**崩 UI | 4 |
| F13 | 并行：一个 `acp:opencode` + 一个 `claude` PTY | 两边互不抢输入；PTY 仍走 xterm | 2 |
| F14 | `agent_write` 误调 ACP id（devtool） | 后端明确错误；会话不损坏 | 1 |
| F15 | OpenCode 未安装时 | `available=false` 或 spawn 中文错误，无白屏 | 1–2 |

### 12.2 显示有没有问题

| # | 检查 | 期望 | Phase |
| --- | --- | --- | --- |
| D1 | 主内容区 | 是 **AcpSessionPanel**，不是空 xterm / 不是闪一下终端 | 2 |
| D2 | 用户气泡 | 发送后立即出现用户文本（乐观或回声） | 2 |
| D3 | 助手流式 | chunk 连续追加；无整段错乱插入、无 JSON 原文刷屏 | 2 |
| D4 | 长输出 | 列表可滚动；默认跟底；用户上滚后不被强制拽回（或提供「回到底部」） | 2 |
| D5 | tool 卡片 | 进行中 / 完成状态可区分；title 可读；失败有错误态 | 2–3 |
| D6 | plan（若 OpenCode 发 plan update） | 条目渲染；不遮挡消息 | 2 |
| D7 | 权限卡 | 不被 Composer 挡住；允许/拒绝可点；一次仅一卡清晰 | 3 |
| D8 | Tab 文案 | running→「运行中」；permission→「待确认」；idle→「空闲」；完成后未读逻辑不与 PTY 冲突 | 2–3 |
| D9 | 上下文 usage（若有 usage_update） | 数值合理或隐藏；不出现 NaN / 满条误报 | 4 |
| D10 | 错误态 | initialize/auth/进程崩溃 → 面板内错误条 + 可读 stderr 尾，不是空白 | 2 |
| D11 | 分屏 | ACP + 编辑器 / ACP + PTY 分屏布局正常，无高度 0 | 2 |
| D12 | 主题/高对比 | 浅色背景（若 app 为浅色）下文字对比度足够；无溢出遮挡 TabBar/Composer | 2 |
| D13 | 切换离开再回来 | 离开 tab 期间的 chunk 仍在，不丢历史（同进程生命周期内） | 2 |
| D14 | **负例** | **不**显示 OpenCode PTY 专属：⚡ variant 标签、Build/Plan 按钮、依赖 `opencode_current_variant` 的 UI | 2 |
| D15 | **负例** | TabBar **不**对 `acp:opencode` 走「无 hook 就纯 PTY 启发式」导致状态乱跳；应 bridge 状态 | 2 |

### 12.3 输入区（Composer）有没有问题

| # | 检查 | 期望 | Phase |
| --- | --- | --- | --- |
| C1 | 目标绑定 | 当前 tab 为 `acp:opencode` 时，发送目标是该 agentId | 2 |
| C2 | Enter 发送 | 触发 `acp_prompt`，不是 `agent_write` | 2 |
| C3 | 发送中 | 进行中可再设计为禁止连发或排队；**禁止**把第二次当 PTY 按键打进空 PTY | 2 |
| C4 | 停止 | turn 中显示停止；点击后 C/F9 | 2 |
| C5 | 清空/草稿 | 发送后输入框清空（或与现网 Composer 一致）；切换 tab 草稿不串到 PTY 会话 | 2 |
| C6 | 权限模式控件 | 若显示 ask/auto/yolo：只改持久化 + Host 策略，**网络/进程侧无 F12 按键序列** | 2–3 |
| C7 | 模型按钮 | MVP 隐藏或 disabled+说明；**不得**跑 OpenCode 命令面板注入 | 2 |
| C8 | ⚡ / Ctrl+T | **不**显示 OpenCode variant 按钮；Ctrl+T **不**向 ACP 会话写 `` | 2 |
| C9 | Build/Plan | **不**显示 | 2 |
| C10 | `/` 补全 | 不出现仅适用于 TUI 的 opencode slash 误导项；或列表为空 | 2 |
| C11 | 多 tab 循环目标（Composer Tab 切终端） | 切到 ACP 再发送 → 消息进 ACP 面板；切到 PTY → 仍 `agent_write` | 2 |
| C12 | 无会话/spawn 失败 | 错误 toast 或占位；Composer 不假死 | 2 |
| C13 | 中文/多行/粘贴 | 与现网 Composer 一致可输入；完整进入 `prompt` text block | 2 |
| C14 | todo 拖拽发送（若产品支持） | `assignTodoAndSend` / `sendPromptToAgent` 对 ACP 走同一 `acp_prompt` 路径 | 2 |
| C15 | **负例埋点** | 开发时日志：ACP 发送路径零 `agent_write` 调用 | 2 |

### 12.4 验收执行方式

1. **Phase 2 出口：** 勾完 D* + C* + F1–F4、F10、F13–F15（无工具也可）。  
2. **Phase 3 出口：** 勾完 F5–F9 + D7 + C6。  
3. **Phase 4 出口：** 勾完 F11/F12 + D9 + 全文回归。  
4. 自动化能覆盖的 F14、mock 权限、沙箱 read → 进 CI；OpenCode 真机项保留手工或 `#[ignore]`。  
5. 结果写入 [`acp-dev-status.md`](./acp-dev-status.md) 或本文件附录 A 下「验收记录」小节（日期、opencode 版本、通过/N/A/失败）。

---

## 附录 A — OpenCode ACP launch 与摸底记录

### A.1 已确认（文档编写时本机）

| 项 | 值 |
| --- | --- |
| 二进制 | `opencode`（例：`/home/hachi/APP/n/bin/opencode`） |
| 版本样例 | `1.18.18` |
| ACP 启动 | `opencode acp` |
| 传输 | stdin/stdout **NDJSON**（官方 CLI 描述） |
| cwd | 进程 cwd，或 `opencode acp --cwd <abs>` |
| 与 TUI 差异 | `acp` help **无** `--model` / `--session` / `--continue`（TUI/run 才有） |
| PTY 对照 | CaPilot `runtime: "opencode"` + `runtimes/opencode.rs`（**另一条轨**） |
| CaPilot ACP id | **`acp:opencode`** |
| 默认 descriptor args | `["acp"]` |

```jsonc
// 内置 / 用户配置最小片段
{
  "id": "opencode",
  "name": "OpenCode (ACP)",
  "command": "opencode",
  "args": ["acp"],
  "env": {},
  "cwd_mode": "session",
  "enabled": true
}
```

### A.2 Phase 0 实测（2026-08-17，opencode 1.18.18）

| 项 | 结果 |
| --- | --- |
| `protocolVersion` 协商 | Client 提 `1` → Agent 回 `1` |
| `agentCapabilities` | `loadSession: true`；`promptCapabilities.image/embeddedContext: true`；`mcpCapabilities.http/sse: true`；`sessionCapabilities: {close,fork,list,resume}`（空对象=支持） |
| `authMethods` | `[{id: opencode-login, name: Login with opencode, description: Run \`opencode auth login\` in the terminal}]` — **非**交互式 RPC login，需用户先 CLI 登录 |
| `agentInfo` | `{name: OpenCode, version: 1.18.18}`（无 title 字段） |
| `session/new` | params: `{cwd, mcpServers: []}`；result: `{sessionId: "ses_…", configOptions: [model, effort, mode, …]}` |
| 换模型 | **`session/set_config_option`** `{sessionId, configId: "model", value: "provider/model"}` 成功；返回更新后的 `configOptions`（含 effort 随模型变化） |
| `configOptions` 样例 id | `model`（select）、`effort`（thought_level select）、`mode`（build/plan select） |
| `session/update` 已见表 | `available_commands_update`, `usage_update{used,size,cost?}`, `agent_thought_chunk`, `agent_message_chunk`, `user_message_chunk`, `tool_call`, `tool_call_update` |
| client `fs/*` / `terminal/*` | 本轮工具（bash/ls）**未**回调 client fs；OpenCode 自带工具执行。Client 仍应实现 fs/permission 以兼容其他 agent / 未来行为 |
| `request_permission` | **本轮未观察到**（工具直接跑完）。Host MVP 仍必须实现 handler；默认 ask |
| `session/cancel` | prompt 进行中发 notification → prompt result `stopReason: "cancelled"` ✅ |
| `session/load` | ✅ 同进程 `session/load` 成功，result 含 `configOptions`（sessionId 在 params） |
| 限流/错误形态 | 部分模型 JSON-RPC `-32603`：`Rate limit exceeded…`（`data.errorName=APIError`）。UI 必须展示，不可静默 |
| 成功 prompt 模型 | `opencode/nemotron-3.5-lightning-free` → 文本 `pong`，`stopReason: end_turn`，usage 含 input/output/thoughtTokens |
| 工具 prompt | `tool_call` title 如 `bash` / `ls /tmp`，kind `execute`，status pending→in_progress→completed + `agent_message_chunk` |
| 样例日志 | `docs/acp-dev-log/20260817-0405-phase0-summary.json`；`20260817-0400-phase0-tool-cancel.json`；`20260817-0355-phase0-opencode-extended.json` |
| mock fixture | `src-tauri/tests/fixtures/mock_acp_agent.py`（initialize/new/prompt 流式 chunk，CI 用） |

### A.3 其他 agent（验收通过后扩展）

| Agent | command | args | 备注 |
| --- | --- | --- | --- |
| Gemini CLI | `gemini` | 查当前版 | 第二候选 |
| Goose | `goose` | 查 | |
| Raxol | `raxol` | `acp` | 文档曾写明 |
| Custom | `node` | `agent.js --acp` | Zed custom 形态 |

Zed custom 参考：

```json
{
  "agent_servers": {
    "my-agent": {
      "type": "custom",
      "command": "node",
      "args": ["~/projects/agent/index.js", "--acp"],
      "env": {}
    }
  }
}
```

### A.4 验收记录（实现后填）

| 日期 | opencode 版本 | §12 通过/失败/N/A | 备注 |
| --- | --- | --- | --- |
| 2026-08-17 | 本机 `opencode`（ACP bootstrap OK） | **代码/mock 路径 PASS**；F5–F9 OpenCode 真 UI **EXEMPT**（Wayland + 进程内工具可能不回调 client） | Phase 0–3 Test `test_passed`（commits 至 `65b851cf9`）。Phase 4：Settings CRUD + usage mirror + RUNBOOK/ai-runtime-references 链接 + resume 路径（`loadSession` 能力见 A.2）。锚点 UI 全闭环仍受 Wayland 限；Host+mock+bootstrap 为门禁。详见 `acp-dev-status.md`。 |

---

## 附录 B — 与现有类型对照（实现备忘）

| CaPilot | ACP / OpenCode |
| --- | --- |
| `AgentSession.id` | Client 侧 tab/agent 句柄，**不是** ACP sessionId |
| `runtime: "acp:opencode"` | descriptor id `opencode` + 前缀 |
| `runtime: "opencode"` | **PTY only**（老会话） |
| `resume_key` | ACP `sessionId` |
| `AgentSession.cwd` | spawn cwd / `acp --cwd` / session/new cwd |
| `mode` ask/auto/yolo | Host permission 策略（非 F12） |
| `speed` / variant / Build·Plan | ACP MVP **忽略**；PTY OpenCode 仍自有 |
| `model` | ACP MVP 忽略或只读；PTY 仍命令面板 |
| `agent_write` | **禁止**用于 ACP；改 `session/prompt` |
| PTY 字节流 | `session/update` 事件流 |
| OpenCode status 插件侧车 | **不用**；`AcpBridge` 内存状态 |
| `XTermPanel` | `AcpSessionPanel` |
| `RuntimeInfo.models` | ACP 空或未来 configOptions |
| `opencode_current_variant` IPC | **仅** PTY `opencode` |

---

## 附录 C — 建议实现顺序（开发者 checklist）

```text
[ ] Phase 0 对本机 opencode acp 摸底 → 填附录 A.2
[ ] acp/ 骨架 + mock 测试绿
[ ] descriptor 默认 acp:opencode；list/spawn 分叉
[ ] 无 UI 真连 OpenCode 一轮 prompt
[ ] AcpSessionPanel + ContentArea（isAcpRuntime）
[ ] sendPromptToAgent / Composer ACP 控件集
[ ] 勾选 §12.2 + §12.3 + F1–F4
[ ] permission 闭环 + OpenCode 真工具 → §12.1 F5–F9
[ ] resume/usage/Settings（若能力允许）
[ ] 全量 cargo test && pnpm tsc --noEmit
[ ] PTY 回归 + §12 总表签字（附录 A.4）
```

---

*本文是实现前的设计基线。验证锚点为 OpenCode ACP。若官方 crate API、wire schema 或 `opencode acp` 行为与上文冲突，以实测与官方协议为准，并回写附录 A 与相关章节。*
