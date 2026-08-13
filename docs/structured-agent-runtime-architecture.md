# 结构化 Agent Runtime 架构开发文档

> 状态：Proposal  
> 日期：2026-08-14  
> 范围：CaPilot IDE 的 Agent Provider 接入、统一会话模型、事件流、权限边界与迁移方案

## 1. 背景

CaPilot 当前把 Claude Code、Codex、OpenCode 等 Agent CLI 当作交互式终端程序运行：Rust adapter 负责拼接启动参数，`PtyBridge`/daemon 托管 PTY，前端通过 xterm 渲染 TUI，并向 PTY 注入文本、Enter、方向键、功能键和 slash command。

该方案能够保留 Provider 原生 TUI，但已经产生以下结构性问题：

- Agent 能力被约束为“终端输入和终端输出”，上层无法可靠获得消息、推理、工具调用、diff、计划、权限等语义。
- 模型、模式和思考强度的切换依赖 CLI 参数、TUI 菜单顺序、快捷键和固定延时。
- 生命周期依赖 Claude/Codex hook、OpenCode 插件、PTY 活动启发式以及侧车文件。
- 会话恢复依赖各 Provider 的 JSONL、SQLite 或 CLI 输出，并存在时间窗口猜测。
- 每增加一个 Provider，都要实现启动、输入、状态、恢复、用量、命令发现和前端特例。
- 原生 TUI 的布局、交互和视觉风格无法统一。

当前实现的主要入口：

- Runtime trait：`src-tauri/src/agent_runtime/adapter.rs`
- Runtime adapters：`src-tauri/src/agent_runtime/runtimes/`
- PTY/daemon bridge：`src-tauri/src/bridge.rs`
- Agent Tauri commands：`src-tauri/src/lib.rs`
- TUI 驱动：`ui/components/layout/Composer.tsx`
- Prompt 注入：`ui/state/agentActions.ts`
- 运行时命令发现：`src-tauri/src/slash.rs`
- 现状中的易变事实：`docs/ai-runtime-references.md`

本方案将 Agent 从终端模型迁移为结构化会话模型。ACP 是首选通用接入协议；当 Provider 的原生结构化协议能提供更完整或更稳定的能力时，使用 Direct adapter。PTY 继续服务普通终端，不再作为 Agent 的统一接入契约。

参考：

- [ACP v1 Overview](https://agentclientprotocol.com/protocol/v1/overview)
- [ACP Session Setup](https://agentclientprotocol.com/protocol/v1/session-setup)
- [ACP Tool Calls](https://agentclientprotocol.com/protocol/v1/tool-calls)
- [Paseo Providers](https://paseo.sh/docs/providers)
- [Paseo Custom Providers](https://paseo.sh/docs/custom-providers)

## 2. 设计目标

### 2.1 必须实现

1. 为所有 Agent Provider 提供统一的创建、恢复、prompt、取消、配置、权限和关闭接口。
2. 将 Provider 原生事件归一化为统一 timeline，前端不感知底层协议。
3. Agent 主界面使用统一的结构化 UI，不渲染 Provider 原生 TUI。
4. 支持任意 ACP stdio Agent 通过配置接入，不修改 Rust 代码。
5. 支持 Claude、Codex、OpenCode 等核心 Provider 使用 Direct adapter 增强能力。
6. daemon 成为 Agent session、权限和事件状态的权威所有者。
7. Provider session ID 通过统一 persistence handle 保存和恢复。
8. Agent 与普通 Terminal 分离，保留 bash/zsh、workspace script 和登录终端。
9. 在 daemon 层实施 session ownership、workspace root 和权限校验。
10. 删除缓存命中率相关的数据采集和 UI。

### 2.2 非目标

- 不统一或代理 Provider 背后的模型 API、订阅和账号体系。
- 不复刻 Claude、Codex、OpenCode 的原生 TUI。
- 不保证所有 Provider 暴露完全相同的可选能力。
- 不把 ACP schema 直接作为 CaPilot 内部领域模型。
- 不在首期实现跨 Provider 会话无损迁移。
- 不把账户剩余用量放入 Agent session 核心接口。
- 不把 ACP permission 当作操作系统沙箱。

## 3. 核心原则

### 3.1 Agent 与 Terminal 分离

Agent 是结构化会话，Terminal 是字节流进程。两者是平级能力：

```text
AgentManager                         TerminalService
    │                                     │
    ├── ACP Provider                      ├── bash/zsh
    ├── Direct Provider                   ├── workspace scripts
    └── canonical timeline                └── 可交互登录/诊断终端
```

Agent UI 不调用 `agent_write`、`agent_resize`，也不依赖 xterm。Terminal UI 仍可使用 PTY 的 write/resize/attach/checkpoint 能力。

### 3.2 内部契约独立于 ACP

ACP 是 Provider adapter 的一种输入协议。CaPilot 的 `AgentEvent`、`TimelineItem`、`PermissionRequest` 和 `PersistenceHandle` 必须保持 Provider-neutral，原因包括：

- Direct adapter 不一定使用 ACP 概念或字段。
- ACP 可选能力并非所有 Agent 都实现。
- ACP v2 当前仍处于 Draft，内部模型不能随 wire protocol 同步破坏。
- CaPilot 需要保存本地 UI 状态、workspace ownership 和事件序号。

首期基于稳定 ACP v1；ACP v2 只能通过版本协商和 feature flag 实验性启用。

### 3.3 Provider adapter 只做翻译

Provider adapter 负责：

- 检测命令和诊断运行环境；
- 获取模型、模式和配置目录；
- 创建/恢复 Provider session；
- 与原生协议通信；
- 将事件转换为 `AgentEvent`；
- 将统一配置、权限响应和取消操作转换回原生请求；
- 返回 Provider 原生 persistence handle；
- 关闭自己持有的进程、连接和 helper service。

Provider adapter 不负责：

- 直接更新前端 store；
- 直接渲染或控制 UI；
- 决定 workspace ownership；
- 写 canonical timeline；
- 根据终端输出猜测全局 Agent 状态；
- 把 Provider 特例扩散到 `Composer.tsx`。

### 3.4 AgentManager 维护权威状态

`AgentManager` 是以下状态的唯一权威来源：

- Agent/session 生命周期；
- foreground turn；
- canonical timeline；
- pending permissions；
- Provider runtime/config snapshot；
- persistence handle；
- attention 状态；
- 事件序号和客户端重连快照。

前端只消费快照和事件，不自行推导 Provider 生命周期。

## 4. 目标架构

```text
┌─────────────────────────────────────────────────────────────┐
│                         CaPilot UI                          │
│  Timeline · Tool · Diff · Plan · Permission · Config       │
└──────────────────────────────┬──────────────────────────────┘
                               │ Tauri IPC / daemon protocol
┌──────────────────────────────▼──────────────────────────────┐
│                          daemon                            │
│  AgentManager · ProviderRegistry · Persistence · Policy    │
└───────────────────────┬───────────────────┬─────────────────┘
                        │                   │
              ┌─────────▼────────┐   ┌──────▼───────────────┐
              │ Generic ACP     │   │ Direct adapters      │
              │ JSON-RPC/stdio  │   │ SDK/RPC/HTTP/SSE     │
              └─────────┬────────┘   └───┬────────┬─────────┘
                        │                │        │
        Gemini/Copilot/OpenCode/...   Claude   Codex ...

┌─────────────────────────────────────────────────────────────┐
│ TerminalService · PtyBridge · resident PTY daemon          │
│ bash/zsh · scripts · interactive auth/diagnostic terminal   │
└─────────────────────────────────────────────────────────────┘
```

### 4.1 daemon 进程职责

Agent Provider 进程和连接必须由 resident daemon 持有，而不是由 WebView 持有。UI 退出或重载后：

- Provider session 可继续运行；
- daemon 继续接收并持久化结构化事件；
- UI 重连时读取 Agent snapshot 和遗漏的 timeline；
- 不需要通过 VT100 screen checkpoint 恢复 Agent 视图。

现有 PTY daemon 可以继续作为 `TerminalService` 基础。Agent 协议应新增结构化的 daemon request/event，而不是把 JSON-RPC bytes 塞入 PTY output channel。

## 5. 统一领域模型

以下代码用于表达目标契约，不要求逐字采用。实际实现应放在无 Tauri UI 依赖的 Rust 模块中。

### 5.1 Provider capabilities

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub session_resume: bool,
    pub session_list: bool,
    pub structured_tools: bool,
    pub reasoning_stream: bool,
    pub permissions: bool,
    pub config_options: bool,
    pub slash_commands: bool,
    pub mcp_servers: bool,
    pub images: bool,
    pub context_usage: bool,
}
```

所有可选 UI 必须由 capability 或实际返回的数据驱动，不能根据 Provider ID 猜测。

### 5.2 Provider catalog

```rust
pub struct ProviderCatalog {
    pub models: Vec<ModelDefinition>,
    pub config_options: Vec<ConfigOption>,
    pub capabilities: ProviderCapabilities,
}

pub enum ConfigOption {
    Select {
        id: String,
        label: String,
        category: Option<String>,
        current: String,
        options: Vec<SelectOption>,
    },
    Boolean {
        id: String,
        label: String,
        category: Option<String>,
        current: bool,
    },
}
```

模型、模式、thinking、sandbox、network 等均优先表示为运行时发现的 config option。UI 可根据 `category` 将常用项放在 Composer，其余项放入 session 设置面板。

### 5.3 AgentClient

```rust
#[async_trait]
pub trait AgentClient: Send + Sync {
    fn provider_id(&self) -> &str;

    async fn is_available(&self) -> Result<bool, AgentError>;
    async fn diagnostic(&self) -> Result<ProviderDiagnostic, AgentError>;
    async fn fetch_catalog(&self, cwd: &Path) -> Result<ProviderCatalog, AgentError>;

    async fn create_session(
        &self,
        config: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError>;

    async fn resume_session(
        &self,
        handle: PersistenceHandle,
        overrides: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError>;
}
```

### 5.4 AgentSession

```rust
#[async_trait]
pub trait AgentSession: Send + Sync {
    fn provider_id(&self) -> &str;
    fn runtime_session_id(&self) -> Option<&str>;
    fn capabilities(&self) -> &ProviderCapabilities;

    async fn start_turn(&self, prompt: AgentPrompt) -> Result<TurnId, AgentError>;
    async fn interrupt(&self) -> Result<(), AgentError>;

    async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError>;

    async fn respond_to_permission(
        &self,
        request_id: &str,
        action_id: &str,
    ) -> Result<(), AgentError>;

    fn describe_persistence(&self) -> Option<PersistenceHandle>;
    async fn close(&self) -> Result<(), AgentError>;
}
```

`close()` 只释放运行资源，不删除 Provider 的原生会话，也不归档 CaPilot Agent record。

### 5.5 Prompt

```rust
pub enum PromptContent {
    Text { text: String },
    Image { mime_type: String, data: Vec<u8> },
    Resource { uri: String, text: Option<String> },
}

pub struct AgentPrompt {
    pub client_message_id: String,
    pub content: Vec<PromptContent>,
}
```

Adapter 必须根据 capability 拒绝不支持的内容类型。不得把图片路径或二进制静默转成任意终端文本。

## 6. 事件与 canonical timeline

### 6.1 AgentEvent

```rust
pub enum AgentEvent {
    SessionReady(SessionReady),
    TurnStarted(TurnStarted),
    Timeline(TimelineEvent),
    PermissionRequested(PermissionRequest),
    PermissionResolved(PermissionResolution),
    ConfigUpdated(Vec<ConfigOption>),
    ContextUsageUpdated(ContextUsage),
    TurnCompleted(TurnCompleted),
    TurnCancelled(TurnCancelled),
    TurnFailed(TurnFailed),
    SessionClosed,
}
```

`ContextUsageUpdated` 是可选 session 能力。账户 quota 不通过该事件上报。

### 6.2 TimelineItem

```rust
pub enum TimelineItem {
    UserMessage(MessageItem),
    AssistantMessage(MessageItem),
    Reasoning(MessageItem),
    ToolCall(ToolCallItem),
    Plan(PlanItem),
    Error(ErrorItem),
}
```

每个 item 必须具有稳定 `item_id`。流式更新使用同一个 ID：

```rust
pub enum TimelineEvent {
    Started { item: TimelineItem },
    Appended { item_id: String, text_delta: String },
    Replaced { item: TimelineItem },
    Finished { item_id: String, status: ItemStatus },
}
```

规则：

- UI 不按文本内容去重。
- 用户提交时由 AgentManager 创建唯一 `client_message_id`。
- Provider echo 通过 correlation metadata 绑定到同一个 canonical user item。
- Tool call 的 pending/running/completed/failed/cancelled 都更新同一 item。
- Provider 私有字段只能放入可选 `metadata`，不能改变通用渲染所需的字段。
- 原始 Provider event 可写 debug trace，但不能作为 UI 的主数据源。

### 6.3 生命周期状态

统一 Agent 状态：

```text
initializing → idle ↔ running
                  ↘ waiting_permission
                  ↘ waiting_input
任意状态 → error
任意 live 状态 → closed
```

- `running` 由 foreground turn 或明确的 Provider work event 驱动。
- `waiting_permission` 由尚未解决的 `PermissionRequest` 驱动。
- `waiting_input` 由结构化 elicitation/question 驱动。
- `idle` 由 turn completion、Provider idle 或 session ready 驱动。
- 不再使用“最近 2 秒有 PTY output”判断 Agent 是否运行。
- `closed` 表示当前无 live runtime，但存在可恢复记录；归档是独立字段。

## 7. Provider Registry

### 7.1 Registry 职责

Registry 将以下信息组合成一个 Provider definition：

- Provider ID 和展示信息；
- adapter 类型；
- 默认 command；
- command/env override；
- 模型覆盖；
- Client factory；
- runtime catalog；
- enabled/order 等用户配置。

静态 manifest 与 runtime catalog 分离：

- manifest：名称、图标、说明、默认 adapter；
- runtime catalog：capability、模型和 config option；
- profile override：command、env、label、模型增补。

### 7.2 配置示例

```json
{
  "providers": {
    "opencode": {
      "adapter": "acp",
      "command": ["opencode", "acp"]
    },
    "gemini": {
      "extends": "acp",
      "label": "Gemini",
      "command": ["gemini", "--acp"]
    },
    "codex-work": {
      "extends": "codex",
      "label": "Codex Work",
      "env": {
        "CODEX_HOME": "/path/to/work-codex-home"
      }
    }
  }
}
```

规则：

- 自定义 Provider ID 必须稳定且可持久化。
- `extends: "acp"` 使用 Generic ACP adapter。
- `extends: <builtin>` 复用 Direct adapter，但 persistence handle 对外仍使用 profile ID。
- command 使用 argv 数组，不使用空格分割字符串。
- env 只注入该 Provider 进程，不修改用户全局环境或配置。
- catalog override 只做展示或显式补充，不能伪造 Provider 不支持的 capability。

## 8. 两种 Provider 接入方式

### 8.1 Generic ACP adapter

首期实现 ACP v1 stdio transport：

```text
spawn configured command
  → initialize
  → capability negotiation
  → authenticate（如需要）
  → session/new 或 session/resume/session/load
  → session/prompt
  ← session/update
  ↔ session/request_permission
  → session/cancel
  → session/close
```

Generic adapter 负责：

- NDJSON JSON-RPC request ID 和 pending response；
- stdout protocol 解码；
- stderr 日志；
- initialize/version/capability 协商；
- session 生命周期；
- ACP content/tool/plan/config/permission 到内部模型的转换；
- 子进程退出、取消和异常清理；
- request timeout 与 backpressure；
- Provider `_meta` 的隔离保存。

任何支持 ACP stdio 的 Agent 应只需配置 command 即可接入。Provider-specific ACP subclass/transformer 仅用于兼容偏差，不能复制一套完整 adapter。

### 8.2 Direct adapter

以下情况允许使用 Direct adapter：

- Provider 没有 ACP；
- ACP 丢失重要结构化能力；
- 原生协议提供更可靠的 session、权限或 tool event；
- 需要复用 Provider server 进程；
- 需要 Provider 原生子 Agent、历史导入或高级配置。

建议接入顺序：

| Provider | 首期方案 | 后续方案 |
| --- | --- | --- |
| OpenCode | `opencode acp`，验证 Generic ACP | ACP 能力不足时再接 HTTP API/Event Stream |
| Codex | Direct `codex app-server` | 保留 `codex-acp` 作为兼容选项 |
| Claude | Claude Agent SDK 或 Claude ACP | 根据 SDK 分发、授权和功能覆盖决定默认值 |
| Gemini/Copilot/Cursor 等 | Generic ACP | 仅对已验证的兼容偏差增加 transformer |
| bash/zsh | 不属于 Provider | 继续由 TerminalService/PTY 托管 |

Direct adapter 必须通过统一 contract test，不得把 Provider 分支带回前端。

## 9. 权限与安全边界

### 9.1 信任模型

```text
WebView/UI            不作为权限权威
daemon                权限、ownership、root scope 的执行者
Provider process      可请求能力，但不能扩大自身 scope
OS/files/network      最终受 daemon policy 与 Provider sandbox 共同约束
```

结构化 permission 解决统一交互和审计问题，不等于 sandbox。部分 Direct Provider 会在自己的进程内直接读写文件或执行命令，此时 CaPilot 仍依赖 Provider 自身的 sandbox/approval 配置。

### 9.2 必须实施的校验

1. 所有 Agent 操作先由 daemon 使用 `agent_id` 查找真实 provider、session 和 workspace；不相信前端传入的 provider/cwd。
2. workspace roots 在创建 session 时 canonicalize，并作为 session 固定 scope。
3. IDE 提供的文件 API 只能访问 session roots；拒绝路径穿越、符号链接逃逸和不存在路径的错误 fallback。
4. IDE 提供的 terminal API 必须验证 session ownership、cwd 和 terminal ownership。
5. `respond_to_permission` 只能选择该 pending request 已声明的 action，并且只能解决一次。
6. permission resolution、文件写入、命令执行和 tool result 记录审计事件。
7. Provider env、token 和 MCP header 不写入 timeline、普通日志或前端 store。
8. IDE/MCP tools 使用 allowlist；Provider capability 不能自动获得任意 Tauri command。
9. 原始 PTY write 只属于 Terminal tab，不对结构化 Agent UI 暴露。
10. kill/close/archive/delete 使用不同语义和 API，禁止混用。

### 9.3 权限模型

```rust
pub struct PermissionRequest {
    pub id: String,
    pub agent_id: String,
    pub kind: PermissionKind,
    pub title: String,
    pub description: Option<String>,
    pub subject: PermissionSubject,
    pub actions: Vec<PermissionAction>,
}

pub struct PermissionAction {
    pub id: String,
    pub label: String,
    pub behavior: PermissionBehavior,
}
```

`PermissionSubject` 至少覆盖 tool call、terminal command、file change、plan/mode change 和 question。Adapter 必须保留 Provider 的原始 action ID，不能只压缩为一个无上下文的 `allow/deny` 布尔值。

## 10. 持久化与恢复

### 10.1 PersistenceHandle

```rust
pub struct PersistenceHandle {
    pub provider_id: String,
    pub runtime_session_id: String,
    pub native_handle: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}
```

`native_handle` 的示例：

| Provider | native handle |
| --- | --- |
| ACP | ACP session ID 和协商版本 |
| Claude | Claude session ID/resume token |
| Codex | thread ID |
| OpenCode | session ID |

### 10.2 Agent record

现有 `AgentSessionRecord` 的 `runtime`、`resume_key`、`mode`、`speed`、`model` 需要迁移为：

```text
provider_id
backend_kind          acp | direct | legacy_pty
workspace_id
cwd
status
config_json
capabilities_json
persistence_json
last_event_seq
created_at
updated_at
archived_at?
```

timeline 应单独持久化，不把完整历史塞入 agent row。SQLite 可继续使用；要求：

- 每个 timeline item 有稳定 ID；
- update/upsert 具备事务性；
- permission pending/resolved 可恢复；
- event sequence 单调递增；
- UI 重连先取 snapshot，再订阅 `seq > snapshot_seq` 的事件；
- Provider 原始事件只能进入可限额的 debug trace。

### 10.3 恢复流程

```text
读取 Agent record
  → ProviderRegistry 解析 provider/profile
  → AgentClient.resume_session(handle)
  → 获取 runtime config/capabilities
  → 订阅 Provider event
  → 对齐 canonical timeline
  → Agent 进入 idle/running/waiting 状态
```

不能把一个 Provider 的 handle 交给另一个 Provider。切换 Provider 不再表示“继续同一个原生会话”：

- 同 Provider profile 内允许恢复；
- 更换 Provider 时创建新 Agent session；
- 可选地把旧会话摘要或选中消息作为新 session 的初始 context；
- UI 必须明确提示这是 handoff，不是无损 runtime switch。

## 11. 前端结构

### 11.1 Tab 类型

```ts
type TabType = "agent" | "terminal" | "editor" | "diff";
```

- `agent`：结构化 timeline；
- `terminal`：xterm/PTY；
- `editor`/`diff`：保持现状。

### 11.2 Agent view

建议组件边界：

```text
AgentPanel
├── AgentTimeline
│   ├── MessageItem
│   ├── ReasoningItem
│   ├── ToolCallItem
│   ├── DiffItem
│   ├── PlanItem
│   └── ErrorItem
├── PermissionPanel
├── SessionConfigBar
└── AgentComposer
```

规则：

- 前端 store 保存 daemon snapshot，不保存 Provider transport 对象。
- `AgentComposer` 只调用 `agent_start_turn`，不调用 `agent_write`。
- 模型/模式/thinking 统一调用 `agent_set_config_option`。
- permission 统一调用 `agent_respond_permission`。
- Esc/停止统一调用 `agent_interrupt`。
- slash command 来自 session command update，不扫描 Provider 私有目录作为主路径。
- Provider-specific metadata 默认不渲染；有明确产品需求时通过受控扩展组件展示。

## 12. 用量模型

### 12.1 删除缓存命中率

以下内容不进入新架构：

- `cache_hit_tokens`；
- `cache_total_input_tokens`；
- Provider 缓存会计口径归一化；
- Composer 的缓存命中率 chip；
- 为读取缓存数据而扫描 Provider transcript/DB 的逻辑。

迁移完成后删除 `ui/components/layout/CacheHitRate.tsx` 及相关 store/type/adapter 字段。

### 12.2 Session context usage

可选保留：

```text
context_window_used_tokens
context_window_max_tokens
```

它属于当前 Agent session，由 `ContextUsageUpdated` 上报。Provider 不提供可信数据时不显示。

### 12.3 Account quota

账户剩余用量属于 Provider/account 层：

```text
ProviderQuotaService
  → fetch on demand
  → plan/window/balance
  → Settings 或状态栏展示
```

它与 Agent session、context window 和缓存命中率无关。若保留现有 `usage.rs`，应重命名和迁移到独立 Provider quota 模块，不作为 `AgentClient` 的必要方法。

## 13. daemon API 草案

结构化 Agent API：

```text
provider_list
provider_refresh_catalog
provider_diagnostic

agent_create
agent_resume
agent_get_snapshot
agent_start_turn
agent_interrupt
agent_set_config_option
agent_respond_permission
agent_close
agent_archive
agent_delete
agent_subscribe
```

Terminal API：

```text
terminal_create
terminal_attach
terminal_write
terminal_resize
terminal_kill
terminal_list
```

不要继续让 `agent_write` 同时代表“发送 Agent prompt”和“向 shell/TUI 注入按键”。

## 14. 迁移计划

### Phase 0：冻结和清理

- 停止为现有 TUI adapter 增加新的按键自动化。
- 删除缓存命中率 UI 与数据字段。
- 将账户 quota 明确标记为 Provider 独立能力。
- 给旧 session 标记 `backend_kind = legacy_pty`。

验收：现有功能不回归，新代码不再依赖 cache 字段。

### Phase 1：领域模型和 AgentManager

- 新建 Provider-neutral 类型和错误模型。
- 实现 AgentManager、Agent record、timeline store 和 event sequence。
- 实现结构化 daemon commands/events。
- 保持旧 PTY runtime 并行运行。

验收：使用 fake provider 可以创建 session、流式写 timeline、取消、请求权限和恢复 snapshot。

### Phase 2：Generic ACP 纵向切片

- 实现 ACP v1 stdio transport。
- 完成 initialize、session/new、prompt、update、permission、cancel、resume/load、close。
- 使用 `opencode acp` 作为首个真实 Provider。
- 建立 ACP conformance/fake agent 测试。

验收：不挂载 xterm 即可完成一次包含 tool call 和 permission 的真实 turn。

### Phase 3：统一 Agent UI

- 增加 AgentPanel 和 canonical timeline renderer。
- 增加统一 Composer、配置选择器和 permission panel。
- 拆分 `agent` 与 `terminal` tab。
- 实现重连 snapshot/event replay。

验收：OpenCode ACP 的消息、工具、权限和结束状态都由统一 UI 呈现。

### Phase 4：核心 Direct adapters

- Codex：`codex app-server` JSON-RPC。
- Claude：评估并接入 Claude Agent SDK；保留 Claude ACP 作为替代。
- 仅在 ACP 缺失产品所需能力时增加 OpenCode Direct adapter。

验收：Direct 和 ACP Provider 通过同一 contract test；前端没有 Provider ID 分支处理核心交互。

### Phase 5：持久化迁移与旧链路退役

- 新 session 默认使用 structured backend。
- 旧 PTY Agent session 保留只读/兼容入口和明确的 EOL 提示。
- 删除 Agent 主路径的 hooks、状态侧车、按键注入和 resume-key 扫描。
- PTY daemon 代码收敛到 TerminalService。
- 删除 `agent_switch_runtime`，替换为显式 handoff/create flow。

验收：Agent 主路径不再依赖 Provider TUI；bash/terminal 功能保持可用。

## 15. 文件级迁移映射

| 当前文件/模块 | 目标 |
| --- | --- |
| `agent_runtime/adapter.rs` | 被新的 `agent_provider` contract 替代；PTY shell adapter 移入 terminal 模块 |
| `agent_runtime/runtimes/*.rs` | 逐步替换为 ACP/Direct provider adapters；旧实现进入 `legacy_pty` |
| `agent_runtime/pty_core.rs` | 保留给 TerminalService |
| `bridge.rs` | PTY 部分保留；新增或拆分结构化 Agent daemon bridge |
| `daemon/protocol.rs` | 新增 Agent request、snapshot、event 和 permission message |
| `lib.rs` `agent_write/resize` | 移为 terminal API；Agent 增加 start_turn/config/permission/interrupt API |
| `persistence.rs` | 增加 provider/config/persistence/timeline schema |
| `Composer.tsx` | 删除 Provider TUI 驱动，改为统一 config/start-turn 操作 |
| `XTermPanel.tsx` | 仅用于 terminal tab 和 legacy PTY session |
| `agentActions.ts` | `sendPromptToAgent` 改调结构化 `agent_start_turn` |
| `slash.rs` | ACP/session command update 为主；本地发现仅作 legacy fallback |
| `status_hooks.rs` | structured Provider 覆盖后删除 |
| `usage.rs` | 拆成可选 Provider quota service |

## 16. 测试策略

### 16.1 Contract tests

所有 adapter 运行同一套行为测试：

- create session；
- start turn；
- assistant streaming；
- tool lifecycle；
- permission request/resolve；
- interrupt；
- config update；
- persistence describe/resume；
- close 后拒绝新请求；
- transport 异常转换为统一 error。

### 16.2 AgentManager tests

- event sequence 单调且无重复；
- timeline item update 不产生重复 item；
- user echo correlation；
- pending permission 持久化与单次解决；
- turn completion/failure/cancel 状态转换；
- UI 重连 snapshot + gap event 无丢失；
- closed、archived、deleted 语义独立。

### 16.3 安全测试

- 使用其他 workspace 的 agent/session ID 被拒绝；
- cwd/path traversal 和 symlink escape 被拒绝；
- terminal ID ownership 校验；
- 未声明 action ID 无法解决 permission；
- secret/env 不进入事件和普通日志；
- structured Agent API 无法调用 raw PTY write。

### 16.4 UI tests

- 同一 fake timeline 在不同 Provider 下渲染一致；
- message/tool/diff/plan/permission 的 streaming 更新；
- capability 缺失时隐藏对应控件；
- 不使用 sleep 驱动 Provider 菜单；
- reload 后从 snapshot 恢复相同 timeline。

## 17. 完成标准

满足以下条件后，结构化 Agent Runtime 可成为默认路径：

1. OpenCode ACP 可完成创建、prompt、stream、tool、permission、cancel 和 resume。
2. 至少一个 Direct adapter 通过相同 contract tests。
3. 前端 AgentPanel 不依赖 xterm 和 ANSI 字节流。
4. 新增通用 ACP Provider 只需配置 command。
5. 模型、模式和 thinking 由 runtime catalog/config option 驱动。
6. daemon 重启或 UI 重连不会丢失 canonical timeline。
7. Agent status 不依赖 PTY activity 或 hook sidecar。
8. Agent prompt 不经过 raw PTY write。
9. workspace/session ownership 和 permission action 在 daemon 校验。
10. 缓存命中率代码和 UI 已删除；账户 quota 与 session usage 分离。
11. Terminal tab 的 bash、resize、attach、kill 和 checkpoint 不回归。

## 18. 待决策项

实施 Phase 2 前确认：

1. Claude 默认采用 Claude Agent SDK 还是 Claude ACP wrapper。
2. structured AgentManager 是否直接进入现有 resident daemon，或先在 Tauri 进程验证后迁移；最终必须由 daemon 持有。
3. timeline 首期使用 SQLite 单表还是 event + snapshot 双表。
4. Provider profile 中的 secret env 使用现有 settings 存储还是系统 keychain。
5. 旧 PTY Agent session 的兼容期限和导出策略。
6. 是否在首期保留 context-window meter；缓存命中率不保留。
7. Provider 原始 debug trace 的默认关闭、限额和脱敏规则。

这些决策不改变核心方向：统一 UI 建立在 Provider-neutral Agent session/timeline 之上，ACP 是默认通用适配器，Direct adapter 用于核心 Provider 的结构化增强，PTY 只承担终端职责。
