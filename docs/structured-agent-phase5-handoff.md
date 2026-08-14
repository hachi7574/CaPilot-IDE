# CaPilot 结构化 Agent Runtime — Phase 5 交接（持久化迁移与旧链路退役）

> 对应 `docs/structured-agent-runtime-architecture.md` §12/§13/§18 与 §746-760 的 Phase 5 部分。本文记录已交付的代码、协议扩展、设计决策与验证结果，供后续（旧 PTY 链路彻底移除、Claude provider 接入、三平台打包）使用。所有路径相对 `src-tauri/`。

> **实现背景**：Phase 5 代码工作由 deepseek-harness agent（`.dsh`，opencode-go/deepseek-v4-flash）在 2026-08-14 晚完成，但其在收尾阶段（写本报告时）LLM 连接持续失败（`Request timed out.` / `Connection error.`，TRANSPORT），会话被中止。本报告由后续会话在**全量验证通过后**补写，验证结果均为本机实测。

## 1. 验收结论

| 验收项（§760） | 结果 |
| --- | --- |
| 新 session 默认使用 structured backend，daemon 暴露 provider `backend_kind` | ✅ `agent_provider_list` 返回 `ProviderInfo{provider_id, backend_kind}`；前端 `createStructuredAgent` 用 `provider.backend_kind`（**不再硬编码 `"acp"`**，修复 Phase 4 handoff §6 记录的接缝） |
| 旧 PTY Agent session 保留只读/兼容入口 + 明确 EOL 提示 | ✅ `agent_write` 对 legacy PTY 会话返回中文 EOL 拒绝；XTermPanel 对 legacy tab 渲染 `readOnly` + `xterm-eol-banner` |
| 删除 Agent 主路径的 hooks、状态侧车、按键注入、resume-key 扫描 | ✅ 侧车轮询/广播注释退役；Composer key-injection 删除；resume-key 扫描由注释明确退役（legacy 路径不再猜 resume key） |
| PTY daemon 代码收敛到 TerminalService | ✅ 新增 `src/terminal/` 模块（`TerminalService` facade + `PtyCore`），`agent_runtime/pty_core.rs` 删除 |
| 删除 `agent_switch_runtime`，替换为显式 handoff/create flow | ✅ Tauri 命令表中已无 `agent_switch_runtime`；LeftSidebar `switchRuntime` → `closeAgent` + 新建 |
| `cargo test` 全绿 | ✅ lib 150 passed / 1 ignored + acp_conformance 4/1 + contract_conformance 4/1 + daemon_smoke 4 passed |
| `pnpm tsc --noEmit` | ✅ exit 0 |
| `cargo fmt --check` | ✅ 通过 |
| `cargo clippy --all-targets` | ✅ **无 error**；39 条 warning 全部位于保留的 legacy `agent_runtime/` EOL 文件与早期文件（风格建议，非阻塞，见 §5） |
| `pnpm build`（vite） | ✅ exit 0（101 modules transformed） |
| bash Terminal 保持可用 | ✅ `src/terminal/` 独立承载 bash/脚本/auth 终端；`daemon_binary_spawn_attach_live_smoke` 回归绿 |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `docs/structured-agent-phase5-handoff.md` | 本文 |
| `src/terminal/mod.rs` | **TerminalService**（§4.1）：daemon 的 PTY facade，own PTY 集合；结构化 Agent 会话**有意不**经它路由（Agent/Terminal 概念分离）。`pub use pty_core::{OnExit, OutputSink, PtyCore, SinkError, SinkResult}` |
| `src/terminal/pty_core.rs` | 自 `agent_runtime/pty_core.rs` 迁移（Tauri 无关的 PTY 生命周期核心：spawn/write/resize/kill/reap、generation 检查、natural-exit 回调、live-slot 预算） |

### 修改

- **`agent_provider/types.rs`**：新增 `ProviderInfo { provider_id, backend_kind }`。
- **`agent_provider/manager.rs`**：`provider_info()` 聚合已注册 provider；`AgentRecord`/`AgentSnapshot` 带 `backend_kind`。
- **`agent_provider/traits.rs`**：`AgentClient` 增 `backend_kind()`。
- **`daemon/protocol.rs`**：`PROTOCOL_VERSION` **3→4**；新增 `RequestCmd::ProviderList`、`RequestCmd::ProviderDiagnostic`、`RequestCmd::ProviderRefreshCatalog`、`RequestCmd::AgentList`；`AgentCreate` 请求带 `backend_kind`；`Response::ProvidersListed{providers}`。多个 roundtrip 断言。
- **`daemon/client.rs`**：新增 `provider_list()` / `agent_create()` / `agent_start_turn()` / `agent_set_config_option()` / `agent_respond_permission()` / `agent_list()` 等结构化 agent 客户端方法。
- **`daemon/server.rs`**：`provider_info()`；agent create/start_turn/config/permission/list 命令直通 `agent_manager`；持久化 `backend_kind` 落库。
- **`daemon/bin.rs`**：`register_provider(AcpClient::new(opencode_profile()))` + `register_provider(CodexClient::new(codex_profile()))`（均懒启动，§5.1）。
- **`lib.rs`**：
  - 新增 Tauri 命令 `agent_provider_list` / `agent_provider_diagnostic` / `agent_provider_catalog` / `agent_create` / `agent_resume_structured` / `agent_snapshot` / `agent_start_turn` / `agent_interrupt_turn` / `agent_set_config` / `agent_respond_permission` / `agent_close_structured` / `agent_list_structured`。
  - **删除** `agent_switch_runtime`。
  - `agent_write`：对 legacy PTY Agent（claude/codex/opencode EOL）返回 `"该会话为旧版 PTY Agent（EOL），已进入只读兼容模式；请新建结构化 Agent 会话"`。
  - resume-key 扫描退役（legacy 会话仅按显式 persistence handle resume）。
  - hook/status 机制清理。
- **`persistence.rs`**：新增 `BACKEND_KIND_LEGACY_PTY = "legacy_pty"`；legacy 会话记录带该标记。
- **`tests/daemon_smoke.rs`**：新增 `daemon_binary_structured_agent_roundtrip`（provider_list 断言 backend_kind=acp → agent_create → snapshot Idle/sequenced → 第二客户端重连读一致）。
- **`ui/state/structuredAgent.ts`**：`createStructuredAgent` 先 `agent_provider_list`，取 `provider.backend_kind`（不再是 `backendKind: "acp"` 硬编码）。
- **`ui/components/terminal/XTermPanel.tsx`**：legacy PTY agent tab → `readOnly`（不转发按键/粘贴/滚动输入）+ EOL banner。
- **`ui/components/layout/LeftSidebar.tsx`**：删除 `switchRuntime`，改显式 close + 新建 flow；agent session 入口文案调整。
- **`ui/components/layout/Composer.tsx`**：删除 Composer key-injection（`sendKeys`/注入逻辑），走结构化 agent channel；净 -795 行。
- **`ui/state/store.ts`**：删除 hook status 轮询与 store 机制（净 -218 行）。
- **`ui/components/layout/TabBar.tsx`**：hook-status 侧车 poll 退役，tab 条直接派生状态。
- **`ui/App.css`**：`xterm-eol-banner` 等样式。
- **`docs/ai-runtime-references.md` / `docs/context-window-usage.md`**：同步删除 cache-hit-rate 文档、legacy 集成事实更新。

## 3. 协议扩展（daemon v4）

```text
RequestCmd::ProviderList            # 列出已注册 provider → Response::ProvidersListed{providers}
RequestCmd::ProviderDiagnostic{provider_id}
RequestCmd::ProviderRefreshCatalog{provider_id, cwd}
RequestCmd::AgentList               # 列出 agent snapshot
RequestCmd::AgentCreate{request}    # NewAgentRequest 现携带 backend_kind
# + AgentStartTurn / AgentSetConfigOption / AgentRespondPermission / AgentClose / AgentResume
Response::ProvidersListed { providers: Vec<ProviderInfo{provider_id, backend_kind}> }
```

- `PROTOCOL_VERSION` 升到 4；`Hello.capabilities` 声明含结构化 agent 能力。
- legacy PTY 会话在 DB 中记录 `backend_kind = "legacy_pty"`，与结构化 (`acp`/`direct`) 区分。

## 4. 关键设计决策

- **backend_kind 来源唯一化**：daemon 的 `provider_info()` 从已注册 provider 客户端派生 `backend_kind`，前端不再硬编码。插一个 provider = 一个 profile 即可（contract test 已保证），UI 无 Provider ID 分支。
- **Agent/Terminal 概念分离落地**：`src/terminal/` 是 TerminalService（bash/脚本/auth 终端）；结构化 Agent 走 `agent_provider`。`agent_runtime/` 保留为 legacy PTY EOL 兼容层，不再进主路径。
- **旧链路只读 EOL，而非立即删除**：legacy PTY Agent 会话（历史已存）保留只读渲染 + `agent_write` 拒绝 + EOL banner；`runtime_list_available` 仍在（兼容）。主路径的 hooks/按键注入/resume-key 扫描已删。
- **契约测试仍双跑**：`contract_conformance.rs` 对 `fake-acp-agent` 与 `fake-codex-app-server` 共用同一场景体，Phase 5 未破坏该不变量。

## 5. 已知边界 / 接缝

1. **legacy `agent_runtime/` 尚未物理删除**：只做了主路径退役与只读 EOL。彻底移除（含 `runtimes/claude.rs`/`codex.rs`/`opencode.rs`/`bash.rs`、`adapter.rs`、`runtime_list_available`、`ENV_LOCK` 测试等）是后续独立任务；届时 lib 测试数会进一步下降（本次已从 Phase 4 的 162 → 150，净 -12，即随 legacy 路径删除的测试）。
2. **clippy 39 条 warning**：全部为保留 legacy 文件与早期文件的风格建议（`new_without_default`、`sort_by_key`、`too_many_arguments`、`doc_lazy_continuation` 等），**无 error**。未在本 phase 扩散修改以避免污染交付面；彻底移除 legacy 时自然消除。
3. **`runtime_list_available` 仍是 legacy runtime 枚举**：前端 SettingsModal/`ui/state/runtime.ts` 仍消费它。若后续决定连 legacy runtime 一并下线，需同步改这两处。
4. **`agent_write` EOL 拒绝基于 backend_kind 判定**：只拦截 legacy PTY 会话；结构化会话走 `agent_start_turn`，不受影响。
5. **本机验证环境**：Wayland 限制 GUI 运行时 spot-check（见 memory）；验证靠 `daemon_smoke` 二进制级冒烟 + 全量编译/测试 + tsc/vite。
6. **deepseek-harness 收尾失败**：代码工作完成，仅本报告缺失；已由本次会话补写并全量复验。

## 6. 验证命令（本机复现）

```bash
cd src-tauri
cargo fmt -- --check                          # 通过
cargo clippy --all-targets                    # 无 error（39 条 legacy/早期风格 warning）
cargo test                                    # lib 150 + acp 4 + contract 4 + daemon_smoke 4 全绿（各 1 ignored）
cd .. && pnpm tsc --noEmit                    # exit 0
pnpm build                                    # vite 构建 exit 0
```

> 未提交任何 commit（保持"先不提交"）。
