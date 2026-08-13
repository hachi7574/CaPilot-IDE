# CaPilot PTY 守护进程改造 — Phase 2 交接（daemon + GUI bridge）

> 对应 `docs/pty-daemon-brief.md` §9.2 与 §11 的 Phase 2 部分。本文记录已交付的代码、不变量与验证结果，供 Phase 3（attach 与可靠恢复）直接使用。所有路径相对 `src-tauri/`。

## 1. 验收结论

| 验收项（§11） | 结果 |
| --- | --- |
| `cargo test` 全绿 | ✅ 115 passed / 1 ignored（唯一 ignored 为 `usage.rs` 手动 opencode cookie 测试，需环境变量，非本次改动） |
| `pnpm build`（含 TypeScript） | ✅ 通过（`tsc --noEmit` + `vite build`，exit 0） |
| 进程内回退模式保持可编译、可测试 | ✅ 整套测试即运行在回退路径上；`bridge.rs` 的 `in_process_spawn_write_kill_roundtrip` 直接覆盖 |
| 一次运行只有一个 PTY owner | ✅ `PtyBridge::start()` 的三级门控（见 §3.1），`Unavailable` 态绝不 fail-open |
| daemon 二进制进程级冒烟 | ✅ `capilot-ide --daemon` 握手/生成/输出/list/shutdown 全通，第二次启动以 `AlreadyRunning` 退出 0 |
| 资源采样归属 | ✅ 定稿为 §7 方案 2：GUI 保留 `ResourceMonitor`，每 tick 经 `PtyBridge::pids()` → daemon `List` |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `src/daemon/protocol.rs` | 帧协议 + `RequestCmd`/`Response`/`ClientEvent`（Phase 2a） |
| `src/daemon/runtime.rs` | 实例锁、token、socket、权限、路径、InstanceLock（Phase 2a） |
| `src/daemon/server.rs` | `DaemonServer`：bind + accept 循环 + 命令分发 + 输出订阅（Phase 2b），18 个测试 |
| `src/daemon/client.rs` | `DaemonClient`：连接握手 + 请求/事件路由（Phase 2a/b） |
| `src/daemon/bin.rs` | `--daemon` 进程入口（`run_daemon_mode`）+ `daemon_base()` + `APP_VERSION`（Phase 2b） |
| `src/bridge.rs` | **GUI ↔ PTY bridge**：`PtyBridge`/`PtyOwner`（Daemon / InProcess / Unavailable）+ `ChannelSink`（回退策略，自 lib.rs 移入），2 个测试 |
| `docs/pty-daemon-phase2-handoff.md` | 本文 |

### 修改

- `src/main.rs`：`--daemon` 参数分派到 `run_daemon_mode()`，GUI 正常走 `run()`。
- `src/lib.rs`：
  - `pub mod daemon; pub mod bridge;`
  - 8 个 `agent_*`/`sessions_delete`/`delete_project` 命令的 PTY 参数从 `State<Arc<PtyManager>>` 改为 `State<Arc<bridge::PtyBridge>>`，调用 `bridge.spawn/write/resize/kill`。
  - `build_and_spawn` 接收 `&Arc<PtyBridge>` + `Channel<Vec<u8>>`；`ChannelSink` 移入 bridge。
  - `run()`：`PtyBridge::start()` 在 `.setup()` 启动，`attach_app` + `start_event_loop` + `manage`；退出处理器改用 `app_handle.state::<Arc<PtyBridge>>().kill_all()`（daemon 模式 = 显式 shutdown）。
- `src/resource.rs`：`start_sampler`/`tick` 改接 `&PtyBridge`（`bridge.pids()`），不再直接依赖 `PtyCore`。
- `src/daemon/bin.rs`：`daemon_base()` 改为由 `persistence::workspace_root().parent()` 推导，不再打开 DB。
- `Cargo.toml` / `Cargo.lock`：无新增依赖。

## 3. bridge 职责与关键不变量

### 3.1 `PtyBridge::start()` — 唯一 owner 的三级门控（§8）

1. 已运行 daemon → 直接 `DaemonClient::connect` 接管。
2. 探 `InstanceLock::try_acquire`：
   - **锁空闲**：立即释放，进入第 3 步（spawn 的 daemon 会重新拿锁）。
   - **锁被持有但连不上**：重试 30×200ms；仍失败 → `Unavailable`（hard，不回退）。
   - **锁不可用**（权限等）→ `Unavailable`。
3. 锁空闲：spawn `current_exe() --daemon`（detached，不 wait/reap），重试连接。
4. daemon 起不来：**重新拿锁**，拿得到 → 进程内回退（`InProcess`，锁被 bridge 持有到退出）；拿不到 → `Unavailable`。

关键点：
- **绝不 fail-open**：`try_connect` 对 `ClientError::NotRunning` 才重试；`Io`/`Handshake`/`Request` 是 hard 错误，立即判 `Unavailable`——原 daemon 可能在跑，不能制造第二个 owner。
- **回退前必须重拿锁**：证明锁空闲才允许进程内模式，避免“旧 daemon 进程内、新会话 daemon”双重所有权（§8）。
- `Unavailable` 态每个命令返回明确错误，不 panic。

### 3.2 `PtyOwner` 三种模式

| 模式 | 语义 |
| --- | --- |
| `Daemon(Arc<DaemonClient>)` | PTY 全在 daemon 进程；bridge 维护 `channels: agent_id → (Channel, generation)` |
| `InProcess(Arc<PtyCore>, InstanceLock)` | 回退：原 GUI 内 PTY；`InstanceLock` 持有到 bridge 析构，期间禁启 daemon |
| `Unavailable(String)` | 硬错误态：spawn/write/resize/kill → `AgentError::PtyError(reason)`；pids 空、kill_all no-op |

### 3.3 daemon 模式事件面（`start_event_loop` → `drain_events` → `handle_event`）

- `ClientEvent::Output` → 查 `channels`，`channel.send(data)`；失败即**移除该订阅**（§4.3：前端 Channel 消失只取消订阅，PTY 在 daemon 里继续跑）。
- `ClientEvent::Exited{agent_id, exit_code}` → 移除 channel + 重新 emit `agent://exited`（与进程内 `build_on_exit` 同 payload 形状）。
- `ClientEvent::Removed` → 移除 channel + 重新 emit `agent://removed`。
- 断连（`Disconnected`）→ 记 error 日志后退出线程（Phase 3 做重连）。

### 3.4 命令映射

- `spawn`：daemon 模式 `client.spawn` → `(pid, generation)`，注册 channel + generation；**`code == "capacity"` 映射回 `AgentError::CapacityReached{limit: MAX_LIVE_SESSIONS}`**，保住 lib.rs 的中文文案“会话数已达上限 (64)”。进程内模式包 `ChannelSink` 走 `PtyCore::spawn`。
- `write`/`resize`：查 `generation`（不在 registry → `AgentNotFound`），走 `client.write/resize`。daemon 协议 `Write` 携带 UTF-8 文本，`write` 入参 `&[u8]` 非 UTF-8 时返回 `PtyError`（进程内模式仍接受任意字节）。
- `kill`：daemon 模式 `client.kill(agent_id, generation: Option<u64>)`，无论成败都清 registry（陈旧条目会错路由后续事件）。
- `pids()`：daemon 模式 `client.list()` → `(agent_id, pid)`，供资源采样；失败返回空。
- `kill_all()`：进程内 `pty.kill_all()`；**daemon 模式 `client.shutdown()`**（§9.2：GUI 退出显式关闭它拉起的 daemon，daemon 内 kill 全部 PTY 后退出）。

### 3.5 会话上限与自然退出

- 上限：daemon 内复用 `pty_core` 的原子 slot reservation（Phase 1 交付，此处未改）；回退模式同一份逻辑。respawn 替换不短暂计两个名额、启动失败释放名额由 pty_core 保证。
- 自然退出持久化：daemon 的 `on_exit` 走 `SessionStore`/`LifecycleJournal`（进程内是 `Persistence`）；GUI 只负责把 `Exited`/`Removed` 事件转成 WebView event，不再重复写 DB（分层原则 §6.1 保持）。

## 4. 测试覆盖矩阵

daemon（18）：

| 测试 | 模块 | 固化行为 |
| --- | --- | --- |
| `frame_roundtrip_…` / `oversized_frame_is_rejected_not_allocated` / `malformed_short_frame_is_rejected` / `output_event_binary_roundtrip_…` / `request_response_tagged_enum_roundtrip` / `hello_and_ack_serialize` | `protocol`(6) | 有界帧协议、事件二进制封包、枚举 roundtrip |
| `token_roundtrip_and_private_file` / `run_dir_is_created_private` / `instance_lock_is_exclusive_and_released_on_drop` / `instance_info_roundtrip` | `runtime`(4) | token 0600、run 0700、实例锁互斥/释放 |
| `handshake_wrong_token_is_rejected` / `instance_lock_prevents_second_daemon` / `spawn_write_list_kill_roundtrip_with_output` / `natural_exit_persists_and_broadcasts_exited_event` | `server`(4) | 鉴权握手、单实例、命令面+输出事件、自然退出持久化+广播 |
| `connect_fails_cleanly_when_no_daemon` / `spawn_write_list_kill_through_client` / `natural_exit_event_arrives_via_client` | `client`(3) | 连接语义、命令 roundtrip、事件送达 |

bridge（2）：

| 测试 | 固化行为 |
| --- | --- |
| `in_process_spawn_write_kill_roundtrip` | 回退模式：spawn → 输出到 Channel → write 回显 → kill → not-found |
| `daemon_mode_routes_output_and_reemits_exit` | daemon 模式端到端：socket 输出 → 事件线程 → Channel；`Exited` 清 registry |

## 5. 验证结果（本次）

```
cargo test:   115 passed; 1 ignored; 0 failed   （ignored = usage 手动 cookie 测试）
cargo check:  全目标无 warning
pnpm build:   tsc ✓ + vite build ✓ (exit 0)

进程级冒烟（target/debug/capilot-ide --daemon，HOME 指向临时目录）：
  handshake ok (instance=…, caps=[basic_io])
  spawned: pid=… generation=1
  list ok: 1 live session(s)
  shutdown ok / daemon-exit=0
  第二次 --daemon → exit 0（AlreadyRunning）
  run/ 权限：socket 0600, token 0600, instance.json 0600, run dir 0700
```

## 6. 已知边界与 Phase 3 预留

1. **PID 复用防失效未做**（§7）：资源采样当前按 `(agent_id, pid)`，未按 generation 失效缓存/样本。daemon 的 `LiveSessionSummary.generation` 与 `OutputHub::last_seq()` 已上协议，Phase 3 attach 时按 generation 键控。
2. **单通道失效不反压 daemon**：`handle_event` 对失败 Channel 只移除 GUI 侧订阅，daemon 侧订阅者（`ClientOutputSub`）仍推送 → 该 agent 输出在 GUI 事件队列累积。这是 §4.3 背压/主动退订，Phase 3 在协议加 `Unsubscribe`。当前前端关闭 tab 会 `agent_kill`/`sessions_delete`，不常触发。
3. **GUI 退出显式关 daemon**（§9.2 语义）：`kill_all` = `client.shutdown`，PTY 随 GUI 退出被结束。**不宣称跨 GUI 存活**——Phase 4 改 detach + 常驻。
4. **写路径 UTF-8 约束**：daemon 协议 `Write` 是 String；`raw: true` 按键直通与控制字符均为合法 UTF-8/单字节，无影响。
5. **断线不重连**：`Disconnected` 只记录并停线程。Phase 3 做自动重连 + `Attach(after_seq)` 补帧。

## 7. 对 Phase 3+ 的接缝

- `OutputHub::last_seq()` / `LiveSessionSummary.last_seq`：`Attach(after_seq)` 的基线已在线。
- `ClientEvent::Output.seq`：补帧序号已在线。
- `daemon/runtime.rs` 的 token/socket/实例锁布局：Phase 4 常驻时复用，不迁移。
- 回退 `ChannelSink` 的 kill 策略（§8 回退模式行为）已明确归属 bridge，daemon 侧不引入。
