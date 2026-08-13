# CaPilot PTY 守护进程改造 — Phase 4 交接（常驻与离线生命周期）

> 对应 `docs/pty-daemon-brief.md` §3/§6.1/§6.2/§7/§9.4 与 §11 的 Phase 4 部分。本文记录已交付的代码、不变量与验证结果，供后续（升级/三平台打包/恢复策略细化）使用。所有路径相对 `src-tauri/`。

## 1. 验收结论

| 验收项（§11） | 结果 |
| --- | --- |
| `cargo test` 全绿 | ✅ lib 146 passed / 1 ignored（唯一 ignored 为 `usage.rs` 手动 opencode cookie 测试）+ 集成冒烟 3 passed |
| `pnpm build`（含 TypeScript） | ✅ 通过（`tsc` + `vite build`，exit 0） |
| 正常/强杀 GUI 后 daemon+agent 存活；重开 attach 到相同 `(daemon_instance_id, agent_id, generation, pid)`，不调 provider resume | ✅ 协议 `Detach` + `spawn_daemon_process` 进程组/SIGHUP 加固；`daemon_smoke.rs::daemon_binary_detach_survives_gui_exit_and_reattaches` 固化（c2 `list()` 断言同 `(generation, pid)`，attach 同代次写回显） |
| 活动 tab ×、`sessions_delete`、项目休眠保持 kill/delete 或 kill/keep-record 语义 | ✅ 前端 kill/delete 路径未改；`kill_all` 保留为显式停 daemon 语义，仅 GUI 退出改 detach |
| 离线期间自然退出 / delete-mode / hook 变化重连可重放，todo 逻辑不漏事件 | ✅ `LifecycleJournal` 记录 + `SyncEvents/EventLog` 补发 + 前端 `agent_sync_events` 重放（`useAgentEvents`），带 `event_seq` 水位去重 |
| 资源面板仍约 3s 更新；PID generation 切换后旧历史不串到新进程 | ✅ `bridge.pids()` → `(agent_id, generation, pid)`，`ResourceMonitor.generations` 换代即清 history/tree |
| daemon 不可启动且无其他 owner 时自动回退进程内；已存在 owner/鉴权失败/协议不兼容不静默回退 | ✅ 沿 Phase 2 三级门控（`PtyOwner::InProcess/Daemon/Unavailable`）；`agent_sync_events` 回退为空，`detach` 在回退模式 kill_all |
| daemon 崩溃 / 应用升级 / 系统注销关机 / 三平台安装包 | ⚠️ **进程边界可验证部分已覆盖**（崩溃→连接 EOF 清租约；强杀 GUI 冒烟测试）；系统注销/关机语义依赖 `SIGHUP` 忽略 + 独立进程组，未做真实注销演练；macOS/Windows 打包在 Linux 环境**无法验证**，见 §11 接缝 |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `docs/pty-daemon-phase4-handoff.md` | 本文 |

### 修改

- `src/daemon/protocol.rs`：`PROTOCOL_VERSION` **2→3**；新增 `CAPABILITY_EVENT_REPLAY`、`EVENT_HOOK_STATUS=4`；`RequestCmd::Detach`、`RequestCmd::SyncEvents{last_seq}`、`Response::EventLog{last_seq, events}`、`JournalEvent` 扁平结构（`seq/ts/agent_id/kind` + `exit_code`/`status` 可选字段）。3 个新 roundtrip 断言。
- `src/daemon/server.rs`：`status_dir` 字段 + `spawn_status_monitor`（500ms 轮询，首扫播种基线不记录）；`record_hook_status`（journal + 广播）；`cmd_detach`（释放租约 + 标记连接关闭）；`cmd_sync_events`（`journal.since(last_seq)` → `EventLog`）；`make_on_exit` 改为**持久化 + journal + 广播三层**，并释放输入租约；`HelloAck.capabilities` 含 `event_replay`。3 个新测试。
- `src/daemon/client.rs`：新增 `detach()`、`sync_events(last_seq) -> SyncEventsResult`。2 个新测试。
- `src/bridge.rs`：`AgentHookStatus{id,status,ts,event_seq}`；`AgentExited`/`AgentRemoved` 增 `event_seq`；`last_event_seq: AtomicU64` 水位；`handle_event` 新增 HookStatus 分支并统一 `fetch_max(event_seq)`；`detach()`（模式感知）；`sync_events()`（daemon 分支 + warn-on-err 回退空）；`pids()` → `Vec<(String,u64,u32)>`；`spawn_daemon_process` 加固（`.process_group(0)` + stderr 重定向 `/tmp/capilot-daemon.log`）。1 个新测试 + 1 个断言更新。
- `src/lib.rs`：`ExitRequested` 由 `kill_all()` 改为 `bridge.detach()`（§9.4）；新增 Tauri command `agent_sync_events(last_seq)` 并注册。
- `src/daemon/bin.rs`：daemon 入口忽略 `SIGHUP`（`libc::signal(SIGHUP, SIG_IGN)`），配合独立进程组，避免随 GUI 会话挂断而退出。
- `src-tauri/Cargo.toml`：新增 `libc = "0.2"`。
- `src/resource.rs`：`generations: HashMap<agent_id, u64>`，`tick()` 内换代即清 history/tree（§7 防 PID 复用）。
- `src/lifecycle_journal.rs`：Phase 1d 已有；Phase 4 补 3 个测试的类型修正（`seq` 计数断言）。
- `tests/daemon_smoke.rs`：新增 `daemon_binary_detach_survives_gui_exit_and_reattaches`（3 个测试总）。
- `ui/state/session.ts`：`useAgentEvents` 重写 —— 注册三个 listener（`agent://exited`/`removed`/`hook-status`）后调 `agent_sync_events` 补发离线事件，按 `seq` 顺序 `applyReplay`（`exited→done`、`removed→closeTab+removeAgent`、`hook_status→setHookStatus`），以 `appliedSeq` 水位去重，仅处理 `s.agents.has(id)` 的已知会话。

## 3. 协议扩展（Phase 4，v3）

```text
RequestCmd::Detach                  # GUI 退出：释放本连接所有输入租约 + 订阅，daemon 保活
RequestCmd::SyncEvents { last_seq } # 拉取 seq > last_seq 的已记录生命周期事件
Response::EventLog {
  last_seq: u64,        # journal 当前高水位
  events: Vec<JournalEvent>,
}
ClientEvent::HookStatus {
  agent_id: String, status: String, ts: i64, event_seq: u64,
}
# ClientEvent::Exited / Removed 各新增 event_seq: u64
```

- `JournalEvent` 是扁平、kind 打标的线形结构（`"exited"|"removed"|"hook_status"`），GUI 可直接重放而不依赖共享 store。
- 版本号升到 3；`Hello.capabilities` 声明 `basic_io + attach + event_replay`。旧客户端（v2，无 detach/replay 面）不会驱动新 daemon。
- `PROTOCOL_VERSION` 文档注释明确：v3 为 Phase 4 的 `Detach`/`SyncEvents` 专用，任何后续改动必须升版本号。

## 4. 生命周期事件流水（§3/§6.1）

```text
自然退出 / delete-mode 清理 / hook 变化
        │
        ▼
  SessionStore.apply_natural_exit()   # 持久化事实源（delete 模式则删记录）
        │
        ▼
  LifecycleJournal.record(...)        # 追加 seq 事件（全局单调，上限 4096）
        │
        ▼
  broadcast_client_event()            # FRAME_EVENT 推给所有 open 连接
        │
        ▼
  GUI handle_event → Tauri emit       # agent://exited / removed / hook-status（含 event_seq）
        │
        ▼
  前端 store 更新（live 路径）
```

- **GUI 在线**：实时事件驱动 store（与回退模式同路径，Phase 4 仅多了 `event_seq` 字段）。
- **GUI 离线**：事件只进 journal；重连后前端先注册 listener，再 `agent_sync_events(lastSeq=appliedSeq)` 拉全窗口并按序 `applyReplay`。DB 恢复已反映 done/removed 事实，回放只补离线窗口，永不复活已删会话。
- **去重**：前端 `appliedSeq` 由 live 事件的 `event_seq` 与回放响应的 `seq` 共同推进；`applyReplay` 幂等（同状态再次 `setHookStatus`/`updateAgentStatus` 无副作用），竞态双应用安全。

## 5. status hook 监测（§3/§10）

- daemon 后台线程轮询 `config.base/status`（500ms），只认 `<agent_id>.json` 的 per-agent sidecar（跳过 `hook*.json`、`.tmp`、空 agent_id）。
- **首扫播种基线、不记录**：GUI 离线前已存在（或 daemon 启动前遗留）的 sidecar 不当作新迁移重放，避免把陈旧 `working` 状态刷给重连 GUI。
- 变化判定 `(status, ts)` 与 `seen` 不符 → `record_hook_status`（journal + 广播）。文件格式与 agent 注入方式未改（§10 约束），daemon 只接管监测/记录，回退模式仍由 GUI 读。

## 6. bridge：`detach` / `sync_events` / `event_seq`

- **`detach()`** 模式感知：daemon → `client.detach()` + `closed=true`；进程内 → `kill_all()`（无 daemon 保活，PTY 随 GUI 死，DB 保持 running，下次启动 resume）；Unavailable → 无操作。
- **`sync_events()`** daemon 分支直通 `client.sync_events`，失败记 warn 并回退空（不把断线误当事件丢失）；其余模式回空。
- `last_event_seq` 水位在 `handle_event` 每个生命周期事件上 `fetch_max`；前端据此跳过已见事件，回放与 live 事件不双计。
- `spawn_daemon_process` 加固：`process_group(0)` 使 daemon 脱离 GUI 的进程组（Ctrl-C/挂断不波及），stderr 指向 `/tmp/capilot-daemon.log`（孤儿 daemon 的 `eprintln` 不会 SIGTTOU/EPIPE 崩掉）。bin.rs 再忽略 `SIGHUP` 双保险。

## 7. 资源采样 generation 失效（§7/§11）

- `bridge.pids()` 返回 `Vec<(String agent_id, u64 generation, u32 pid)>`，daemon 模式取 `list()` 的 generation，进程内模式取 `pty.generation()`。
- `ResourceMonitor.generations` 记录每个 agent 最近见过的 generation；`tick()` 发现换代（respawn 新进程）→ 立即清该 agent 的 history ring + 进程树，旧进程样本不串入新曲线，PID 复用也不误报。
- 方案二（§7：GUI 保留 ResourceMonitor，每 tick 从 daemon List 拿 `(agent_id, pid, generation)`）即本实现；采样仍留在 GUI 每 3s 一次。

## 8. 测试覆盖矩阵

daemon server（11，Phase 4 新增 3）：

| 测试 | 固化行为 |
| --- | --- |
| `detach_releases_lease_keeps_session_live` | `Detach` 释放本连接全部租约；会话仍 live，第二客户端可接管租约写入 |
| `sync_events_replays_journal_and_watermark` | `SyncEvents(a)` 只回 `seq>a`，且 `last_seq` 为当前水位 |
| `status_monitor_seeds_baseline_then_records_transitions` | 首扫播种不记录；sidecar 变化后 journal+广播 |

daemon client（6，Phase 4 新增 2）：

| 测试 | 固化行为 |
| --- | --- |
| `detach_releases_lease_then_second_client_takes_over` | 客户端 `detach()` 后另一客户端取得租约 |
| `sync_events_returns_replay_and_watermark` | 客户端侧 `SyncEvents` roundtrip + 水位 |

bridge（5，Phase 4 新增 1）：

| 测试 | 固化行为 |
| --- | --- |
| `sync_events_replays_daemon_journal_and_is_empty_in_process` | daemon 模式回放 journal；进程内/Unavailable 回空 |
| （断言更新）`pids()` 元组形状 | `(id, _, _)` 解构，daemon/进程内两模式均带 generation |

集成冒烟 `tests/daemon_smoke.rs`（3，Phase 4 新增 1）：

| 测试 | 固化行为 |
| --- | --- |
| `daemon_binary_detach_survives_gui_exit_and_reattaches` | **GUI 退出（detach）→ daemon+agent 存活 → 新 GUI 连接 `list()` 同 `(generation, pid)`（无 respawn）→ attach 同代次 → 写回显 → shutdown exit 0** |
| `daemon_binary_spawn_attach_live_smoke` | Phase 3 回归：spawn→banner→attach checkpoint→租约移交→live echo→exit 0 |
| `daemon_binary_wrong_token_is_rejected` | Phase 2 回归：错 token 被拒 |

前端 `ui/state/session.ts`：`useAgentEvents` 重放路径经 `pnpm build`（tsc）类型校验；store `HookStatus{status,ts}` 与回放 `{status, ts}` 逐字段匹配（`setHookStatus` 值比较去重）。

## 9. 验证结果（本次）

```
cargo test --lib:    146 passed; 1 ignored; 0 failed   （ignored = usage 手动 cookie 测试）
cargo test --test daemon_smoke:  3 passed; 0 failed
cargo check --all-targets: 无 warning
pnpm build:   tsc ✓ + vite build ✓ (exit 0)

二进制级冒烟（target/debug/capilot-ide --daemon，HOME 指向临时目录）：
  c1 spawn(sh) → __READY__
  c1.detach() + drop（GUI 退出）
  c2 connect → list() 断言 sessions[0].(generation, pid) == 原值（无 respawn）
  c2.attach(同 generation) → checkpoint 重建 → write "ping" → got:ping
  c2.shutdown → daemon exit 0
```

## 10. 已知边界

1. **生命周期 journal 是内存态**（`MAX_JOURNAL_EVENTS = 4096`，front 丢弃）。daemon 重启后 journal 清零，靠 DB 行 + live daemon snapshot 对账（§6.1），离线重放只对“当前 daemon 运行期内”的事件负责。
2. **强杀 GUI 的清理依赖 TCP EOF**：daemon 在连接断开时释放租约/订阅；无心跳协议，瞬时断线不会自动重连（沿 Phase 3 已知边界）。
3. **系统注销/关机未做真实演练**：语义依赖独立进程组 + SIGHUP 忽略；Linux 终端注销的实际行为需在目标桌面环境手工验证一次。
4. **三平台打包未验证**：本机为 Linux；`process_group(0)`/`SIGHUP` 是 Unix-only（`#[cfg(unix)]`），macOS 可复用，Windows 需 named-pipe + 任务计划/job 对象策略（见 §11）。
5. **`agent_sync_events` 参数名**：前端传 `lastSeq`，Tauri v2 默认 camelCase→snake_case 映射到 `last_seq`；已在 `pnpm build` 类型层通过，未做 GUI 运行时 spot-check（Wayland 限制，见 memory）。

## 11. 对后续的接缝

- **升级安全（§8）**：v3 握手已有 `CAPABILITY_EVENT_REPLAY`；若未来再扩展，`PROTOCOL_VERSION` 4 需在握手层拒绝混版本组合，且先停旧 daemon 再启动新版本。
- **三平台打包（§4.1/§9.4）**：daemon 需进 bundle（`externalBin` 或同二进制 daemon 模式），macOS 签名/公证、Windows 签名 + 当前用户 ACL named pipe 均未在 CI/本机验证；`docs/ai-runtime-references.md` 与 `docs/CaPilot-IDE-RUNBOOK.md` 待补 daemon sidecar 打包章节。
- **恢复策略（§3）**：daemon 崩溃后 `list` 不可 attach 的会话应核验进程 identity 后返回 `NotLive`、阻止自动 respawn（避免双 agent）—— 本次交付保留 `AgentNotFound → respawn` 沿 Phase 3 语义，identity 核验细节（PID + 启动时间 + generation 三要素）留给后续。
- **断线重连（§4.3）**：`OutputHub::attach(after_seq)` 与 `bridge.last_seq` 已具备补帧能力，连接层自动重连 + 重新 attach 是下一个自然接缝。
