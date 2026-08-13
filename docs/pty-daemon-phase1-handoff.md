# CaPilot PTY 守护进程改造 — Phase 1 交接（行为固化与共享层抽取）

> 对应 `docs/pty-daemon-brief.md` §9.1 与 §11 的 Phase 1 部分。本文记录已交付的代码、不变量与验证结果，供 Phase 2（daemon + GUI bridge）直接使用。所有路径相对 `src-tauri/`。

## 1. 验收结论

| 验收项（§11） | 结果 |
| --- | --- |
| `cargo test` 全绿 | ✅ 93 passed / 1 ignored（唯一 ignored 为 `usage.rs` 手动 opencode cookie 测试，需环境变量，非本次改动） |
| `pnpm build`（含 TypeScript） | ✅ 通过（`tsc` + `vite build`，exit 0） |
| 两个进程并发读写 `sessions.db` + 同一 `.agent-meta.json` 压力测试 | ✅ 由 Phase 1c 的 `persistence::tests` 覆盖（见 §4） |
| 64 会话上限并发压测、失败 spawn 释放名额 | ✅ `pty_core` 的 `capacity_cap_is_enforced_and_failed_spawns_release_quota` |
| 启动修复收敛到 DB 事实源 | ✅ `SessionStore::repair` + 启动时调用 + `repair_recreates_missing_sidecar` |
| 进程内回退模式保持可编译、可测试 | ✅ 整套测试即运行在进程内路径上 |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `src/agent_runtime/pty_core.rs` | 无 Tauri 依赖的 PTY 生命周期核心（原 `pty.rs` 的替代），8 个测试 |
| `src/output_hub.rs` | 每 agent 序号化输出扇出，3 个测试 |
| `src/session_store.rs` | 无 Tauri 依赖的会话持久化门面（DB + sidecar 双写、自然退出策略、修复），3 个测试 |
| `src/lifecycle_journal.rs` | 追加式序号化生命周期事件日志，2 个测试 |
| `docs/pty-daemon-phase1-handoff.md` | 本文 |

### 删除

- `src/agent_runtime/pty.rs`（被 `pty_core.rs` 取代）。

### 修改

- `src/agent_runtime/mod.rs`：`pub mod pty_core;`。
- `src/agent_runtime/adapter.rs`：`AgentError` 新增 `CapacityReached { limit: usize }`。
- `src/lib.rs`：`ChannelSink`（回退模式桥接策略）、`build_and_spawn` 改为接收 `Arc<dyn OutputSink>`、`build_on_exit` 持久化下沉到 `apply_natural_exit`、启动修复、模块可见性 `pub`。
- `src/persistence.rs`：`Persistence` 改为包一层 `SessionStore + LifecycleJournal`。
- `src/resource.rs`：`PtyManager` 引用改到 `pty_core`。
- `Cargo.toml`：无新增依赖（`fs2` 等已在 Phase 1c 加入）。

## 3. 共享层模块的职责与关键不变量

### 3.1 `agent_runtime/pty_core.rs`

- `MAX_LIVE_SESSIONS = 64`，以原子 slot reservation（`compare_exchange_weak`）强制，**计数包含 in-flight spawn**；失败 spawn / 取消通过 RAII `SlotGuard` 释放名额。修正了原 GUI 侧先 `live_count() >= 64` 再 spawn 的跨 agent id TOCTOU。
- `OutputSink` trait：`Send + Sync + 'static`，`send(&self, data) -> SinkResult`。`SinkError::{Closed, Saturated}`（`Saturated` 留给 Phase 3 背压，当前 `#[allow(dead_code)]`）。
- **sink 失败只 detach 该 sink，绝不 kill 子进程**（§2.2）：reader 在 `send().is_err()` 时 `sink = None` 并继续读、丢弃，保证子进程不被写侧阻塞。这与“杀进程”策略分层——后者是 GUI bridge 的策略，不是 pty_core 的。
- reader 线程用 `std::thread::Builder::spawn`（tokio-free，原 `spawn_blocking` 的 abort 本来就是 best-effort）。
- 保留并锁定原 `pty.rs` 竞态语义：
  - Bug 1：`insert` 在 reader 启动前完成；
  - Bug 2：`write` 克隆 writer Arc 后释放全局锁，再在每 agent writer mutex 上阻塞；
  - Bug 4：spawn token 取消；
  - Bug 5：generation 校验淘汰陈旧 reader；
  - 显式 kill 通过 `killed` 标志抑制自然退出回调；kill 后 entry 移除 → `SlotGuard` drop 释放名额。
- `OnExit = Arc<dyn Fn(String, i32) + Send + Sync>`，仅在“自己的 entry 且非 kill”时触发。

### 3.2 `output_hub.rs`

- `AgentOutputHub` 实现 `pty_core::OutputSink`：`send()` 先分配序号再 `subs.retain(|s| s.on_output(...).is_ok())`，**失败订阅者只移除自己**；无订阅者时仍 `Ok`（吸收输出，客户端断开不会级联成 pty_core 杀子进程）。`last_seq()` 是 Phase 3 attach 的基线。

### 3.3 `session_store.rs`

- `SESSION_END_MODE_KEY = "session_end_mode"`：自然退出时 `"delete"` → 删行 + 删 agent 目录；否则行留、状态 → `done`，并同步 `.agent-meta.json`（DB 是事实源，sidecar 是派生副本）。
- `from_base(base)` / `open()`（默认 `~/CaPilot`）/ `db()` / `db_tolerant()`。
- `apply_natural_exit(agent_id) -> NaturalExit { project, deleted }`：每次调用重读设置，设置变更即时生效；best-effort，DB/sidecar 瞬时失败不把自然退出变硬错误。
- `repair()`：以 DB 行重建缺失/损坏的 `.agent-meta.json`，幂等。
- 跨进程锁、WAL、`busy_timeout` 在 `persistence.rs` 的 `SessionsDb` / `AgentMetaGuard` 里（Phase 1c 交付，此处复用）。

### 3.4 `lifecycle_journal.rs`

- 追加式、全局单调序号（1-based）。`record(agent_id, kind, payload) -> seq`，`since(after)` 返回 `seq > after` 的事件（Phase 4 离线重放的窗口），`last_seq()`。`LifecycleEventKind::{Exited, Removed, HookStatus}`。当前为进程内内存日志；磁盘布局 / 保留策略是 Phase 4 决策。

### 3.5 `persistence.rs` 重构

- `Persistence { store: SessionStore, journal: LifecycleJournal }`，`db()`/`db_tolerant()` 委托给 `store`，新增 `store()` / `journal()` / `apply_natural_exit(agent_id, exit_code)`（记录 `Exited{exit_code}` 或 `Removed` 事件后交给 `store.apply_natural_exit`）。
- **分层原则（§6.1）**：`build_on_exit` 只负责持久化 + Tauri `emit`；带 `tauri::AppHandle` 的逻辑不能下沉进共享层。daemon 复用 `SessionStore`/`LifecycleJournal` 时不需要 Tauri。

### 3.6 `lib.rs` 桥接（回退模式策略，非 pty_core）

- `ChannelSink { agent_id, channel, pty }`：`channel.send(data).is_err()` → `let _ = self.pty.kill(&self.agent_id); Err(Closed)`。这是 §8 “Channel 发送失败则终止会话”的回退模式策略，**只存在于 GUI bridge**。
- 三处 `build_and_spawn` 调用点（spawn / resume / switch_runtime）都包了 `ChannelSink`。
- `build_on_exit`：`persistence.apply_natural_exit(&agent_id, exit_code)` 后，按 `deleted` 分别 emit `agent://removed` / `agent://exited`。
- `CapacityReached { limit }` → 中文文案“会话数已达上限 (64)，请先关闭部分终端”。
- 启动时 `if let Err(e) = persistence.store().repair() { log::warn!(...) }`。

## 4. 测试覆盖矩阵（对应 §9.1 固化的行为）

`pty_core`（8）：

| 测试 | 固化的行为 |
| --- | --- |
| `fast_exit_leaves_no_stale_entry_and_fires_on_exit` | 自然退出：entry 清除 + `on_exit` 携带真实 exit_code |
| `blocking_write_does_not_hold_global_lock` | Bug 2：512KB 阻塞写不阻塞对其他 agent 的 kill |
| `sink_failure_detaches_without_killing_child` | §2.2：sink 失败只 detach，live 计数不变 |
| `fallback_policy_kills_on_sink_failure_and_suppresses_on_exit` | 回退模式杀进程策略 + 自然退出回调抑制 |
| `concurrent_same_id_spawn_is_serialized` | 同 id 并发 spawn 串行化（spawn token / Bug 4） |
| `kill_then_respawn_stale_reader_keeps_new_entry` | Bug 5：generation 淘汰陈旧 reader |
| `explicit_kill_suppresses_natural_exit_callback` | 显式 kill 抑制自然退出回调 |
| `capacity_cap_is_enforced_and_failed_spawns_release_quota` | 64 上限严格成立 + 失败 spawn 释放名额 |

共享层（8）：

- `session_store`（3）：keep→done 双写同步 / delete→行+目录删除 / repair 幂等重建。
- `lifecycle_journal`（2）：序号单调 + `since` 过滤 / kind 与 payload 保留。
- `output_hub`（3）：序号单调扇出 / 失败订阅者移除不伤 hub / 无订阅者仍 Ok。

持久化（Phase 1c，13）：含 `two_process_db_write_stress_no_sqlite_busy`、`two_process_meta_lock_prevents_lost_updates`、`old_schema_db_is_migrated`、`update_agent_meta_serializes_concurrent_writers`。

## 5. 验证结果（本次）

```
cargo test:   93 passed; 1 ignored; 0 failed   （ignored = usage 手动 cookie 测试）
pnpm build:   tsc ✓ + vite build ✓ (exit 0)
```

## 6. 对 Phase 2+ 的硬约束（daemon 必须遵守）

1. **只有一个 PTY owner**（§8）：一次运行只选进程内或 daemon 一种模式；daemon 接管后禁止 fail-open 回进程内。
2. **sink 失败 ≠ 子进程致命**（§2.2）：daemon 侧订阅者失败只移除订阅者；只有显式 `Kill` / `sessions_delete` / 项目休眠 / daemon 显式关闭才终止 PTY。`ChannelSink` 的 kill 策略**不得**进入 daemon。
3. **上限与名额**（§7）：daemon 内复用 `pty_core` 的 slot reservation；respawn 替换已有 slot 不短暂计两个名额；新进程启动失败必须释放。
4. **持久化分层**（§6.1）：daemon 用 `SessionStore`/`LifecycleJournal`，不得把带 `AppHandle` 的逻辑搬进共享层；DB 是事实源，sidecar 是可修复派生副本，双写必须走 `AgentMetaGuard` 跨进程锁。
5. **生命周期事件**：`agent://exited` / `agent://removed` / hook 状态变化写入 `LifecycleJournal`，GUI 离线后按 `seq > last_acked` 补发（Phase 4）。`HookStatus` 变体已预留在 journal 中。
6. **序号基线**：`OutputHub::last_seq()` 与 `LifecycleEvent.seq` 是 attach（Phase 3）与离线重放（Phase 4）的接缝，daemon 的 `Attach(after_seq)` / `AckEvents(through_seq)` 应直接映射到这两个模块。
7. **`AgentError::CapacityReached`** 已存在于 adapter，GUI 映射为中文文案；daemon 侧遇满也应返回可分类错误。
