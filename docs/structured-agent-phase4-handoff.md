# CaPilot 结构化 Agent Runtime — Phase 4 交接（核心 Direct adapters）

> 对应 `docs/structured-agent-runtime-architecture.md` §8.1/§8.2 与 §746-752 的 Phase 4 部分。本文记录已交付的代码、协议映射、不变量与验证结果，供后续（Claude provider 接入、Phase 5 持久化迁移与旧链路退役）使用。所有路径相对 `src-tauri/`。

## 1. 验收结论

| 验收项（§752） | 结果 |
| --- | --- |
| Direct（Codex）和 ACP（OpenCode）Provider 通过**同一 contract test** | ✅ `tests/contract_conformance.rs`：同一场景体对 `fake-acp-agent` 与 `fake-codex-app-server` 双跑全绿（full turn / interrupt / resume / foreign-handle 四个场景） |
| 前端没有 Provider ID 分支处理核心交互 | ✅ 结构化 Agent 路径（`ui/state/structuredAgent.ts`、`ui/components/agent/AgentPanel.tsx`）按动态 `provider_id` 索引 catalog/diagnostic/记录渲染，无 `=== "codex"` 类分支（仅旧 PTY TUI 路径保留按 runtime 分支，属旧链路，不在结构化作用域） |
| `cargo test` 全绿 | ✅ lib 162 passed / 1 ignored + acp_conformance 4 passed / 1 ignored + contract_conformance 4 passed / 1 ignored + daemon_smoke 4 passed |
| `pnpm tsc --noEmit` | ✅ 通过（exit 0） |
| `cargo clippy --all-targets` | ✅ 无 error；仅早期 phase 文件的 `doc_lazy_continuation` 文档注释 warning（29 条，均为旧代码注释，非本 phase 新增代码） |
| `cargo fmt` | ✅ 已格式化 |
| 真实 `codex app-server` 冒烟（ignored 测试） | ✅ `contract_live_codex_catalog` 对 **codex-cli 0.147.0** 实测通过：`initialize` → `thread/start` → `model/list` → `thread/unsubscribe`，零 model token |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `src/agent_provider/rpc_stdio.rs` | **共享 JSON-RPC-over-NDJSON stdio 传输层**（§8）。`RpcConnection`（spawn/send_request/timeout/async/notification/respond/respond_error/shutdown）、`Inbound{Notification,Request,ConnectionClosed}`、`RpcError`、`Pending` 回调表、reader/drainer 线程、EOF 时清空 pending + `ConnectionClosed`。ACP 与 Codex Direct 共用；协议无关。 |
| `src/agent_provider/direct/mod.rs` | Direct adapter 模块入口（§8.2 说明 + `pub use codex::{codex_profile, CodexClient, CodexProfile}`） |
| `src/agent_provider/direct/codex.rs` | Codex `app-server` Direct adapter：`CodexProfile`/`codex_profile()`/`CodexClient`/`CodexSession` |
| `src/bin/fake_codex_app_server.rs` | 确定性 fake Codex app-server（contract test 专用 `[[bin]]`，不进入生产路径） |
| `tests/contract_conformance.rs` | 共享契约测试（同一场景体跑 ACP + Direct 两个 provider） |
| `docs/structured-agent-phase4-handoff.md` | 本文 |

### 修改

- `src/agent_provider/mod.rs`：新增 `pub mod direct;` 与 `pub mod rpc_stdio;`
- `src/agent_provider/acp/client.rs`：改为共享传输层别名（`pub use ... RpcConnection as AcpConnection` + `Inbound/RpcError/CLOSE_TIMEOUT`）
- `src/agent_provider/acp/protocol.rs`：`RpcError` 改为 re-export（消除重复定义）
- `src/daemon/bin.rs`：daemon 注册 `CodexClient::new(codex_profile())`（与 OpenCode ACP 并列，均懒启动）
- `src-tauri/Cargo.toml`：新增 `[[bin]] fake-codex-app-server`

## 3. Codex Direct adapter 设计

### 3.1 协议面（`codex app-server --listen stdio://`，v2 schema）

| 客户端方法 | 用途 |
| --- | --- |
| `initialize` | 握手（live 实测接受含 `clientInfo/clientCapabilities/clientVersion/config` 的参数） |
| `thread/start {cwd}` | 新 thread → `{thread.id}` 作为 `runtime_session_id` |
| `thread/resume {threadId, cwd}` | 恢复持久化 thread |
| `model/list` | 拉模型目录（握手时缓存为 `ConfigUpdated` 的 options） |
| `turn/start {threadId, clientUserMessageId, input}` | 发起回合（async，响应由 `turn/completed` 收尾） |
| `turn/interrupt {threadId, turnId}` | 中断 |
| `thread/settings/update {threadId, model}` | 模型切换（`set_config_option`） |
| `thread/unsubscribe` | 优雅关闭 |

通知映射：`item/started`/`item/completed`（agentMessage/reasoning/commandExecution/fileChange/plan）→ `TimelineEvent::Started/Replaced/Finished`；`item/agentMessage/delta`、`item/reasoning/textDelta` → `Appended`（未收到 Started 时合成 Started）；`item/commandExecution/outputDelta` 累积到 `tool_output`；`thread/tokenUsage/updated` → `ContextUsageUpdated`；`turn/completed{status}` → `completed→TurnCompleted / interrupted→TurnCancelled / failed→TurnFailed`。

### 3.2 权限词汇归一化（契约关键）

代码对四种 codex 决策做了**同 ACP 一致的域词汇**映射，这是两份 adapter 通过同一 contract test 的基础：

| 域 action id | codex decision |
| --- | --- |
| `allow_once` | `accept` |
| `allow_always` | `acceptForSession` |
| `reject_once` | `decline` |
| `reject_always` | `cancel` |

`item/commandExecution/requestApproval` / `item/fileChange/requestApproval` 应答 `{decision}`；`item/permissions/requestApproval`（Grant）应答 `{permissions, scope:"turn"}`（逐项 `result.decision` 置 `approve/deny`）。

### 3.3 不变量

- **interrupt 原子换位**：`ActiveTurn.terminal_emitted` 的 `swap` 让 `interrupt()` 与竞态的 `turn/completed` 互斥，恰好一次终态事件（contract `interrupt` 场景断言 1×TurnCancelled / 0×TurnCompleted）。
- **`cancel_requested` 标记**：interrupt 置位、下次 `start_turn` 清除；批准请求在此之后（turn 已取消）到达时**立即应答 `cancel`**，避免服务端挂起无人应答的 approval。
- **服务端 turn id 绑定**：`server_turn_id` 由 `turn/started` 绑定，`turn/interrupt` 与 `turn/completed` 用其校验收束归属。
- **`SessionReady` 权威**：`thread/started` 通知被忽略，避免与客户端自报的 ready 重复。
- **handle 归属校验**：`resume_session` 拒绝 `handle.provider_id != self.profile.provider_id`（与 manager 的 provider 解析双重防护）。

## 4. 共享契约测试

`tests/contract_conformance.rs` 的场景体对 `fake-acp-agent`（ACP v1）与 `fake-codex-app-server`（Codex app-server JSON-RPC）**逐字同一逻辑**：

1. **full turn**：create → prompt → tool call → permission 请求（4 actions）→ `allow_once` 应答 → 工具完成带输出、assistant 增量拼成 `"Hello world"`、恰好 1 条 client 侧 user 消息、`ContextUsageUpdated`、Idle、close 后 Closed。
2. **interrupt**：等到 permission 后 cancel → 恰好 1×`TurnCancelled`、0×`TurnCompleted`。
3. **resume**：从 handle 恢复，`runtime_session_id` 与原始一致。
4. **foreign handle**：`provider_id` 不匹配 → `provider not registered`。

通过契约即等价于 UI 层可互换：核心交互无需 Provider ID 分支。

## 5. Claude Agent SDK 评估（本 phase 仅评估，不接入）

架构 §749 要求"评估并接入 Claude Agent SDK；保留 Claude ACP 作为替代"；§18.1 待决策项仍悬置。评估结论（供 Phase 5+ 决策）：

- **SDK 形态**：Claude Agent SDK 是 TypeScript/Node 编程库（面向 agent loop 编排），不是像 `codex app-server`/ACP 那样的 stdio 协议服务端，与当前 Rust 适配器模型不直接对齐。
- **接入路径**：要么 (a) 起一个 Node sidecar 包装 SDK 并对外讲 JSON-RPC/ACP over stdio（等于造一个自定义 ACP provider，仅需 profile + 能力映射即可通过 contract test）；要么 (b) 等 Claude Code 的 ACP 支持稳定后，作为第二个"通用 ACP provider"接入（同 OpenCode ACP，零新代码）。
- **建议**：本 phase **不接入**。理由：① 不引入 Node 运行时依赖（当前后端纯 Rust）；② ACP 路径的复用成本显著更低，且契约测试已保证"插一个 provider = 一个 profile"；③ §18.1 决策（SDK vs ACP）应在产品形态确认后定。接入时唯一要做的是把 codex 的决策/事件映射翻译成 Claude 的（与 ACP 对齐的四 action 词汇已就绪）。
- **不引入 OpenCode Direct**：OpenCode 由 ACP 覆盖，无产品能力缺口（§750 条件不满足）。

## 6. 已知边界 / 接缝

- **前端新建 agent 的 `backend_kind` 默认 `"acp"`**（`ui/state/structuredAgent.ts` createStructuredAgent）。对 codex 记录实际是 `direct`。当前 UI 不按 `backend_kind` 分支（显示 `provider_id`），无功能影响；若后续要展示 Direct/ACP 徽标，需 daemon 暴露 provider 的 backend 类型（Phase 5 随持久化一起做）。
- **interrupt 早于 `turn/started` 到达**：`turn/interrupt` 需服务端 turn id，极端竞态下可能只能靠"应答 cancel 批准"兜底；`cancel_requested` 已把该窗口收敛到"turn 已取消则后续 approval 一律 cancel"，服务端不会永久挂起。真实 codex 在中断时也会自行取消 pending approval。
- **`fetch_catalog` 用临时 `thread/start` 探活** `model/list`；真实 codex 实测可行（0.147.0），但依赖 server 允许无鉴权 thread；如未来要求登录，catalog 回落为空列表（不 panic）。
- **clippy**：29 条 `doc_lazy_continuation` warning 均位于早期 phase 文件（`persistence.rs`/`bridge.rs`/`daemon/protocol.rs` 等）的文档注释，非本 phase 新增代码；未在本 phase 扩散修改以避免污染交付面。

## 7. 验证命令（本机复现）

```bash
cd src-tauri
cargo fmt
cargo clippy --all-targets        # 无 error
cargo test                        # lib 162 + 集成 12 全绿
cargo test --test contract_conformance -- --ignored   # 真实 codex 0.147.0 catalog 冒烟
cd .. && pnpm tsc --noEmit        # 通过
```

> 未提交任何 commit（保持"先不提交"）。
