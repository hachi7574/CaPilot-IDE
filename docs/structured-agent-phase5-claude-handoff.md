# CaPilot 结构化 Agent Runtime — Claude Provider 接入交接（SDK sidecar）

> 本文记录 Claude Code 结构化接入的交付：**Node sidecar（Claude Agent SDK）→ 复用 codex app-server wire schema → Rust `direct/claude.rs` 薄封装**。这是 Phase 5 handoff §5 列出的后续任务（"Claude provider 接入"），对应架构 §8.1 的 SDK 评估结论——Claude Code 无 ACP，走非 ACP 路径。所有路径相对 `src-tauri/`。

## 1. 背景与路线

- **Claude Code 不支持 ACP**（2.1.232 无 acp 子命令/flag），无法像 opencode 那样"一个 profile 即插即用"。
- 评估结论（Phase 4 §5）：Claude Agent SDK 是 Node 库，非 stdio 协议服务端。可行路线两条：(a) Node sidecar 包装 SDK 对外讲 JSON-RPC/ACP；(b) 等 Claude ACP。
- **本交付选 (a)**：`scripts/claude_agent_server.mjs` 用 Node SDK（`@anthropic-ai/claude-agent-sdk` 0.3.232）把 Claude 会话桥接成**与 `codex app-server` 完全相同的 NDJSON JSON-RPC wire schema**。因此 Rust 侧 **零新增 session 逻辑**——`ClaudeClient` 是 `CodexClient` 的薄包装（复用它经过契约验证的 `CodexSession` 事件处理），插一个 provider 即达成。

## 2. 验收结论

| 验收项 | 结果 |
| --- | --- |
| daemon 注册 claude provider，`agent_provider_list` 返回 `backend_kind=direct` | ✅ `daemon/bin.rs` 注册 `ClaudeClient::new(claude_profile())`（懒启动）；UI 由 provider_list 动态驱动，无需分支 |
| Claude 通过共享契约测试 | ✅ `contract_conformance.rs` 新增 `fake_claude_spec`，同一场景体对 **fake-acp / fake-codex / fake-claude 三份双跑**：full turn / interrupt / resume / foreign-handle 全绿 |
| 真实 Claude 全链路可用（Rust → sidecar → SDK → LLM） | ✅ ignored live smoke `contract_live_claude_turn` 实测：assistant 文本 `"pong"`，TurnCompleted，`ContextUsageUpdated` |
| 真实 sidecar catalog（零 token） | ✅ ignored `contract_live_claude_catalog` 实测通过 |
| 零 token 握手 | ✅ sidecar probe：initialize / thread/start / model/list / settings/update / unsubscribe 全通过，不 spawn SDK |
| `cargo test` 全绿 | ✅ lib 150/1 + acp 4/1 + contract 4/3 + daemon_smoke 4 |
| `cargo clippy --all-targets` | ✅ **无 error**；39 条 warning 全部位于保留的 legacy `agent_runtime/` 文件（本交付新代码 0 warning） |
| `cargo fmt -- --check` | ✅ |
| `pnpm tsc --noEmit` / `pnpm build` | ✅ / ✅ |

## 3. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `scripts/claude_agent_server.mjs` | **Claude SDK sidecar**：stdin 读 NDJSON JSON-RPC，把 SDK `query()` 消息流映射成 codex 事件流；`canUseTool` 权限回调 ↔ `item/commandExecution/requestApproval` 往返；`Query.interrupt()` 实现 `turn/interrupt`；thread↔Claude session 映射持久化到 `~/.capilot/claude-sidecar-sessions.json`（跨 daemon 重启 resume） |
| `scripts/package.json` + `scripts/node_modules/` | sidecar 依赖 `@anthropic-ai/claude-agent-sdk@^0.3.232`（`npm install` 到 `scripts/`；node_modules 已被根 `.gitignore` 忽略） |
| `src/agent_provider/direct/claude.rs` | `ClaudeProfile`/`claude_profile()`（`["node", "<manifest>/scripts/claude_agent_server.mjs"]`）/`ClaudeClient`：`backend_kind()="direct"`，`is_available` 检查 node + 脚本存在，其余委托内部 `CodexClient`（fetch_catalog / create_session / resume_session） |

### 修改

- **`src/agent_provider/direct/mod.rs`**：`pub mod claude;` + re-export。
- **`src/daemon/bin.rs`**：注册第三个 provider `ClaudeClient::new(claude_profile())`。
- **`tests/contract_conformance.rs`**：`fake_claude_spec`（用 `fake-codex-app-server` 作 command，证明 Claude 走 codex wire 即过契约）；四个场景数组加 claude；新增两个 ignored live smoke（`contract_live_claude_catalog` 零 token、`contract_live_claude_turn` 真实 turn）。

## 4. 关键设计决策

- **复用 codex wire schema，而非再造 ACP provider**：sidecar 讲 codex `app-server` 的语言（`initialize`/`thread/start`/`thread/resume`/`model/list`/`turn/start`/`turn/interrupt`/`thread/settings/update`/`thread/unsubscribe` + 事件通知），Rust 侧直接复用 `CodexClient`/`CodexSession`（契约测试已证明"通过契约即 UI 可互换"）。不引入第二个协议面。
- **backend_kind = `"direct"`**：Claude 走 direct 类（非 ACP），与 codex 并列；UI 显示 provider_id（`claude`），不按 backend_kind 分支。
- **thread id = 本地 uuid + 持久化映射**：`thread/start` 不 spawn SDK（**零 token catalog**）；首次 `turn/start` 才创建 SDK session，从 `system/init` 拿真实 `session_id` 绑定到 thread，写入 `~/.capilot/claude-sidecar-sessions.json`。`thread/resume` 时若内存无该 thread（daemon 重启）→ 查映射恢复 `session_id` → SDK `options.resume` 续接历史。实测 resume 跨"重启"成功。
- **SDK 消息 → codex 事件映射**：
  - `assistant` 消息（每条一个完整 block）：text → `item/agentMessage/delta`；thinking → `item/reasoning/textDelta`；`tool_use` → `item/started`（Bash→commandExecution，Write→fileChange，其余→commandExecution 描述）。
  - `user` 消息 `tool_result` → `item/completed`（带 `aggregatedOutput`）。
  - `result` → `turn/completed`（`interruptRequested`→`interrupted`，`is_error`→`failed`，否则 `completed`）+ `thread/tokenUsage/updated`。
  - 对同一 block 用 `msgId:seq` 稳定 item_id + 长度 diff，兼容完整块与增量两种 SDK 行为。
- **权限闭环**：SDK `canUseTool` → sidecar 发 `item/commandExecution/requestApproval`（挂起）→ 客户端回 `{decision}` → 映射 `accept→allow`、`acceptForSession→allow+updatedPermissions(suggestions)`、`decline/cancel→deny`。turn 结束时清空未决 approval（resolve deny），SDK 不挂起。
- **interrupt 恰好一次**：`turn/interrupt` → `Query.interrupt()` → SDK 收尾 yield `result` → sidecar 发 `turn/completed{status:"interrupted"}`；Rust 侧 `finalize_turn` 的 atomic-swap 保证不与竞态完成重复发终态。probe 实测 interrupted 恰好 1 次、无重复 completed。

## 5. 已知边界 / 接缝

1. **sidecar 依赖 node + SDK**：需 `cd src-tauri/scripts && npm install`。缺 SDK 时 `contract_live_claude_turn` 会 fail（而非 skip——`is_available` 只查 node+脚本，不查 SDK 可 import）。daemon 诊断会提示安装命令。
2. **model/list 为硬编码 Claude 模型目录**（opus-5 / sonnet-5 / haiku-4-5）。SDK 若不识别所选模型，turn 会 failed；默认不传 model 用用户全局配置。
3. **映射文件 best-effort**：`~/.capilot/claude-sidecar-sessions.json` 写失败只降级 resume（新会话仍可用）；换机器/清目录后旧 thread resume 失败。
4. **非 Bash/Write 工具**（Read/Glob/Grep/WebFetch/Task…）渲染为 commandExecution 描述，不单独建 item 类型；`fileChange` 只带 `changes.filePath`（不渲染 diff）。
5. **legacy `agent_runtime/` 未物理删除**（同 Phase 5 边界）；`runtime_list_available` 仍是 legacy 枚举，与本交付无关。
6. **`scripts/package-lock.json` 会随提交进入仓库**（可复现安装记录；node_modules 已忽略）。
7. **本机验证**：Wayland 限制 GUI 运行时 spot-check（memory）；验证靠二进制级 live smoke + 全量编译/测试。

## 6. 验证命令（本机复现）

```bash
cd src-tauri/scripts && npm install                     # 装 SDK（一次性）
cd ../..
cargo fmt -- --check                                    # 通过
cargo clippy --all-targets                              # 无 error（39 条 legacy 风格 warning）
cargo test                                              # lib 150 + acp 4 + contract 4 + daemon_smoke 4 全绿
cargo test --test contract_conformance -- --ignored     # 3 个 live smoke：codex catalog + claude catalog(零token) + claude 真实turn
cd .. && pnpm tsc --noEmit && pnpm build                # exit 0 / exit 0
```

> 未提交任何 commit（保持"先不提交"）。
