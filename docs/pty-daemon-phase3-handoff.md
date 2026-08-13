# CaPilot PTY 守护进程改造 — Phase 3 交接（attach 与可靠恢复）

> 对应 `docs/pty-daemon-brief.md` §4.2/§5/§6.3 与 §11 的 Phase 3 部分。本文记录已交付的代码、不变量与验证结果，供 Phase 4（常驻与离线生命周期）直接使用。所有路径相对 `src-tauri/`。

## 1. 验收结论

| 验收项（§11） | 结果 |
| --- | --- |
| `cargo test` 全绿 | ✅ 138 passed / 1 ignored（唯一 ignored 为 `usage.rs` 手动 opencode cookie 测试，需环境变量，非本次改动）+ 集成冒烟 2 passed |
| `pnpm build`（含 TypeScript） | ✅ 通过（`tsc` + `vite build`，exit 0） |
| 进程内回退模式保持可编译、可测试 | ✅ 整套测试运行在回退路径上；`bridge.rs` 的 `in_process_spawn_write_kill_roundtrip` 直接覆盖 |
| attach 期间持续输出无字节丢失/重复 | ✅ OutputHub 单锁原子快照+订阅（§4.2）+ bridge `last_seq` 去重，双测试固化 |
| 主/备屏、跨 chunk 的 UTF-8/转义序列、不同尺寸重建正确 | ✅ `vt_checkpoint.rs` 9 测试 + `output_hub.rs` 重建 roundtrip 覆盖 |
| 一次运行只有一个 PTY owner | ✅ 沿 Phase 2 三级门控不变，attach 路径不新建 PTY，仅接管 live 会话 |
| daemon 二进制进程级冒烟（含 attach） | ✅ `daemon_smoke.rs` 2 测试：spawn→banner→第二客户端 attach（checkpoint 重建）→live echo→shutdown exit 0；错 token 被拒 |

## 2. 交付物清单

### 新增

| 文件 | 内容 |
| --- | --- |
| `src/daemon/vt_checkpoint.rs` | **VT checkpoint 子系统**（Phase 3a）：`render_checkpoint(parser) -> Vec<u8>`，序列化主/备屏切换 + 清屏 + 当前屏 + 光标位 + 样式，含隐藏光标与 UTF-8 跨 chunk 恢复，9 个测试 |
| `src/output_hub.rs`（自 Phase 1 抽取，Phase 3b 扩展） | **快照/增量/原子 attach**：`AgentOutputHub::attach(after_seq) -> AttachSnapshot{snapshot_seq, checkpoint, replay}`；VT parser 逐 chunk 消费 + 有界增量日志（`MAX_INCREMENT_BYTES = 512KiB`）；单锁下快照+订阅原子完成 |
| `tests/daemon_smoke.rs` | **二进制级冒烟**（进程边界）：spawn 真实 `capilot-ide --daemon`（HOME 重定向），走 `DaemonClient` 全链路；错 token 原始 socket 握手被拒。2 测试 |
| `docs/pty-daemon-phase3-handoff.md` | 本文 |

### 修改

- `src/daemon/protocol.rs`：`PROTOCOL_VERSION` **1→2**；新增 `CAPABILITY_ATTACH`；`RequestCmd::Attach{agent_id, generation, rows, cols, after_seq}`、`Response::Attached{snapshot_seq, checkpoint, replay}`（checkpoint/replay 为 `Vec<u8>` 的 JSON 数组）。1 个新 roundtrip 测试。
- `src/daemon/server.rs`：新增 `cmd_attach`（§5：generation 校验 → pty+hub resize 至客户端几何 → `hub.attach` → 输入租约移交 → `Response::Attached`）；`make_on_exit` 捕获 `leases`，自然退出时释放租约；`cmd_resize` 先 `hub.resize`；`cmd_write` 增加租约门控（foreign writer → `lease_held`）。4 个新测试。
- `src/daemon/client.rs`：新增 `DaemonClient::attach(...) -> Result<AttachResult>`（`AttachResult{snapshot_seq, checkpoint, replay}`）。1 个新测试。
- `src/bridge.rs`：新增 `attach_lock: Mutex<()>`（spawn/attach 与事件线程互斥，封死 attach 窗口竞态）与 `last_seq: Mutex<HashMap<agent_id, (generation, seq)>>`（同代 seq 去重、换代重置）；`handle_event` 全程持 `attach_lock` 并按 `(generation, seq)` 去重；新增 `PtyBridge::attach`（daemon 侧以 `client.list()` 为 liveness 权威，`not_found`/`stale_generation` → `AgentNotFound`；进程内/Unavailable → `AgentNotFound`/`PtyError`）。2 个新测试。
- `src/lib.rs`：`agent_resume` **Attach-first**（§6.3）——先 `bridge.attach`，成功则以持久化记录补齐 `runtime/workspace_id/project/mode/speed/model/title/cwd`；仅 `AgentNotFound`（daemon 已回收 / 进程内回退）才 kill + `build_and_spawn` 重续会话。
- `ui/components/terminal/XTermPanel.tsx`：`agent_resume` 调用新增 `rows: term.rows, cols: term.cols`，以实际终端尺寸走 attach（缺省 24×80）。
- `src/output_hub.rs`：重写 `resize_updates_parser_size_for_checkpoint`，用参考 parser 全量 roundtrip 对比（原先硬编码光标位置的断言对 LF-only 行为脆弱）。

## 3. 协议扩展（Phase 3c）

```text
RequestCmd::Attach {
  agent_id: String,
  generation: u64,        // spawn 时返回的代次，防止跨代误接管
  rows: u16, cols: u16,   // §5 initial_size：checkpoint 按客户端几何渲染
  after_seq: Option<u64>, // 客户端已消费到的输出序号（None = 全新终端）
}
Response::Attached {
  snapshot_seq: u64,          // parser 当前已消费到的序号，live 增量从此之后
  checkpoint: Option<Vec<u8>>,// 全量重建（主/备屏切换+清屏+当前屏+光标）
  replay: Vec<u8>,            // after_seq 之后的缺口原始字节（空 = 已全量覆盖）
}
```

- 版本号升到 2；`Hello.capabilities` 同时声明 `basic_io` + `attach`，客户端按 `CAPABILITY_ATTACH` 决定能否用 attach 流程。
- checkpoint/replay 直接以 JSON `Vec<u8>`（int 数组）承载，避免 `base64` 依赖；帧体上限 16MiB 对 512KiB 增量 + 全屏 checkpoint 富余。

## 4. server `cmd_attach` 与输入租约

### 4.1 `cmd_attach` 时序（§5）

1. **代次校验**：`pty.generation(&agent_id)` 与请求一致；`None`/代次不符 → `not_found`/`stale_generation`，**绝不跨代接管**。
2. **`pty.resize(rows, cols)` + `hub.resize(rows, cols)`**：先把 PTY 与 VT parser 调到客户端几何，再生成 checkpoint —— 这保证重建屏与客户端同尺寸（§5 的 `initial_size` 先于快照）。
3. **`hub.attach(sub, after_seq)`**：单锁内读 seq → 渲染 checkpoint 或收集缺口 → 注册订阅者，原子返回 `AttachSnapshot`；此后订阅者只收 `seq > snapshot_seq` 的 chunk。
4. **输入租约移交**：`leases[agent_id] = 新连接`。spawner 不再是默认写者，attach 后由最新客户端输入。
5. 返回 `Response::Attached`。

### 4.2 输入租约语义（§4.2）

- 单写者：`cmd_write` 校验 `leases[agent_id]` 指向的连接持有者；foreign writer → `lease_held` 错误。
- 释放时机：持有连接关闭、`kill`、自然退出（`make_on_exit` 已捕获 `leases`）。关闭态持有者视为空闲。
- attach 的 `list` 先行（bridge 侧）保证“attach 的目标确实 live”，杜绝接管已回收会话。

## 5. bridge：`attach_lock` + `last_seq` 去重

### 5.1 attach 窗口竞态（§4.2）

GUI 事件线程在 daemon 连接上收 `Output` 事件并转发 Channel。attach 与 spawn 若与事件线程并发，会出现 checkpoint 与 live 事件交错的丢字节/重复：

- 新增 `attach_lock: Mutex<()>`：
  - daemon 模式 `spawn` 与 `attach` 全程持锁（从发请求前到 `channels` 注册完成）。
  - `handle_event` 处理**每个**事件前取同一把锁 —— 因此 attach 窗口内到达的事件在无界 mpsc 里排队，attach 完成后事件线程拿到锁，再按去重规则转发，不会先于 checkpoint 送达前端。

### 5.2 `last_seq` 去重（同连接多订阅者）

单条 daemon 连接上的多个 attach（同一 agent 被多个 tab 打开）会收到同一份输出事件。`last_seq: HashMap<agent_id, (generation, seq)>`：

- 每个 `Output` 事件按 `(generation, seq)` 与记录比较，`seq <= last` 跳过。
- spawn 时初始化 `(generation, 0)`；attach 成功时置为 `(generation, snapshot_seq)` —— checkpoint/replay 已送达的内容绝不重发（§11 no-loss/no-dup）。
- 换代（respawn 新 generation）自动重置，旧代次事件整体丢弃。
- `Exited`/`Removed`/`kill` 清理 channel + `last_seq` 条目，防止陈旧路由。

### 5.3 `bridge.attach` 错误映射

| daemon 返回 | bridge 映射 | 语义 |
| --- | --- | --- |
| `not_found` / `stale_generation` | `AgentNotFound` | 已回收或换代 → `agent_resume` 落回 respawn |
| 其他 `ClientError` | `PtyError` | 硬错误，不 respawn |
| `PtyOwner::InProcess` | `AgentNotFound` | 回退模式无 attach，统一走 respawn |
| `PtyOwner::Unavailable` | `PtyError` | 保持 Phase 2 硬错误语义 |

## 6. `agent_resume` Attach-first（§6.3）

```text
读持久化记录
  → bridge.attach(id, rows, cols, on_data)          # Attach-first
    ├─ Ok(info)  → 用持久化记录补齐 info 元数据，返回  # 不 respawn，PTY 原样继续
    └─ Err(AgentNotFound)
        → bridge.kill(id)   # 清残留
        → build_and_spawn(resume=true, resume_key)    # 只有 daemon 真回收才重续
```

- **liveness 权威**：`bridge.attach` 先 `client.list()` 确认 live，再发 `Attach`。daemon 已回收（`Exited` 后清理）→ `list` 找不到 → `AgentNotFound`。
- 前端 `XTermPanel` 传 `term.rows/cols`；`agentActions.ts` 的 `ensureAgentChannel`（无 tab 打开时的后台保活）缺省 24×80，安全（无人看屏）。
- 该命令把“恢复一个运行中的会话”从**重建**改为**接管**：会话状态、TUI 屏、输入租约全部保留，开销只是一个 `Attach` roundtrip。

## 7. OutputHub attach 与 VT checkpoint 细节

- **为何要 VT parser**：裸增量 ring buffer 无法在任意字节处重建屏幕 —— 截断流可能断在 UTF-8/CSI 中间、或依赖更早的屏幕状态（主/备屏、样式、光标）。vt100 parser 逐 chunk 消费，`attach` 时在完整解析边界渲染 checkpoint。
- **`attach(after_seq)` 三种情况**：
  - `None`：全量 `render_checkpoint(parser)`，`replay` 空。
  - `Some(a)` 且 `a > snapshot_seq`：客户端超前，checkpoint 与 replay 都空（服务端不产生负缺口）。
  - `Some(a)` 且日志仍连续覆盖到 `a+1`：只回放缺口字节；否则（日志超限被裁剪、客户端落后太多）退化为全量 checkpoint（§5“超限时生成新 checkpoint 后才能丢弃旧增量”）。
- `resize` 同步 pty 与 parser：checkpoint 反映最新几何；`resize_updates_parser_size_for_checkpoint` 用参考 parser 全量 roundtrip 固化。
- checkpoint 序列化：主/备屏切换（`\x1b[?1049h`/`\x1b[?1049l`）+ 清屏 + 当前屏内容 + 光标位 + 隐藏光标状态；样式经 `contents_formatted` 保留，wide/unicode 逐 cell 校验。

## 8. 测试覆盖矩阵

vt_checkpoint（9）：

| 测试 | 固化行为 |
| --- | --- |
| `empty_screen_roundtrips` / `main_screen_text_and_cursor_roundtrip` / `styles_roundtrip` / `wide_and_unicode_roundtrip` / `hidden_cursor_is_preserved` | checkpoint 序列化→重建与源屏一致 |
| `alt_screen_roundtrip` / `main_after_alt_exits_roundtrip` | 备屏进入/退出后重建正确 |
| `utf8_split_across_chunks_is_fully_recovered` | 跨 chunk 的 UTF-8 断点不损字节 |
| `resize_after_checkpoint_does_not_corrupt` | resize 后重建不破坏 |

output_hub（11，Phase 3 新增/改写 6）：

| 测试 | 固化行为 |
| --- | --- |
| `sequences_are_monotonic_and_fan_out_to_all_subscribers` / `failing_subscriber_is_removed_not_the_hub` / `no_subscribers_is_ok_never_a_sink_error` / `beginning_subscriber_replays_the_log_in_seq_order` / `increment_log_is_bounded` | Phase 1 不变量：单调序号、单订阅者失败不影响 PTY、日志有界 |
| `attach_without_baseline_returns_full_checkpoint` / `attach_with_after_seq_replays_only_the_gap` / `attach_with_stale_after_seq_falls_back_to_checkpoint` / `attach_after_seq_equal_to_snapshot_has_no_gap` | **attach 四态**：无基线全量 / 缺口补帧 / 落后超限退化全量 / 恰好当前无缺口 |
| `resize_updates_parser_size_for_checkpoint` / `full_checkpoint_reconstruction_roundtrips` | resize 后 checkpoint 几何正确；全屏重建 roundtrip |

daemon server（8，Phase 3 新增 4）：

| 测试 | 固化行为 |
| --- | --- |
| `attach_fresh_client_gets_checkpoint_then_live_only` | 全新客户端 attach：checkpoint 重建 banner，此后只收 live |
| `attach_with_after_seq_replays_only_the_gap` | 带基线 attach：仅回放缺口，无重复 |
| `input_lease_transfers_on_attach_and_rejects_foreign_writer` | attach 后租约移交；旧写者被 `lease_held` 拒绝 |
| `attach_rejects_stale_generation_and_not_found` | 跨代/幽灵 attach 被拒 |

daemon client（4，Phase 3 新增 1）：`attach_returns_checkpoint_and_rejects_stale_generation` — 客户端侧 Attach roundtrip + 代次拒绝。

bridge（4，Phase 3 新增 2）：

| 测试 | 固化行为 |
| --- | --- |
| `daemon_attach_roundtrip_checkpoint_and_deduped_live` | GUI 侧 attach：checkpoint+live 经 Channel 送达且无重复 |
| `attach_not_found_in_daemon_and_in_process` | daemon 侧已回收 / 进程内回退 → `AgentNotFound` |

protocol（8，Phase 3 新增 1）：`attach_request_and_attached_response_roundtrip` — `RequestCmd::Attach`/`Response::Attached` 枚举 roundtrip。

集成冒烟 `tests/daemon_smoke.rs`（2）：

| 测试 | 固化行为 |
| --- | --- |
| `daemon_binary_spawn_attach_live_smoke` | 真实 `--daemon` 进程：spawn→banner→第二客户端 attach（checkpoint 经 vt100 重建含 `__READY__`）→租约移交写回显→shutdown exit 0 |
| `daemon_binary_wrong_token_is_rejected` | 原始 socket 错 token 握手 → `FRAME_ERROR` → 连接关闭；正确 token 仍可连 |

## 9. 验证结果（本次）

```
cargo test:   138 passed; 1 ignored; 0 failed   （ignored = usage 手动 cookie 测试）
              tests/daemon_smoke.rs: 2 passed; 0 failed
cargo check:  全目标无 warning
pnpm build:   tsc ✓ + vite build ✓ (exit 0)

进程级冒烟（target/debug/capilot-ide --daemon，HOME 指向临时目录）：
  spawn(sh) → __READY__ banner 到 c1
  第二客户端 attach(24,80) → checkpoint 重建 banner（vt100 校验含 __READY__），replay 空
  c2 持租约写 "ping" → live 回显 got:ping 仅一次
  shutdown → daemon exit 0；错 token 连接被 FRAME_ERROR 拒后关闭
```

## 10. 已知边界与 Phase 4 预留

1. **GUI 退出仍显式关 daemon**：`kill_all = client.shutdown` 沿 Phase 2 语义未改 —— 本阶段“会话跨 GUI 重启存活”依赖 GUI 不主动关 daemon。**Phase 4 改 detach + 常驻**：GUI 退出时 `Detach`（释放订阅与租约）而非 shutdown，daemon 保活。
2. **断线不重连**：`Disconnected` 仍只记日志停线程；attach 已具备 `after_seq` 补帧能力，但连接层没有自动重连+重新 attach。Phase 4 的离线重放/常驻衔接点即在此。
3. **`after_seq` 由 GUI 侧默认传 `None`**：前端当前没有跨会话的 seq 记忆，attach 一律全量 checkpoint。增量补帧路径在 server/client/output_hub 三层已就绪并有测试，等待 Phase 4 的断线重连场景接入。
4. **无心跳/存活检测**：daemon 无法区分“GUI 暂时离开”与“GUI 死亡”，租约一直留在断连连接上直到 TCP 错误触发清理。Phase 4 需在协议加保活或断连即释放租约的策略。

## 11. 对 Phase 4+ 的接缝

- `RequestCmd::Attach` + `Response::Attached`：断线重连时客户端持 `last_seq` 重放缺口，能力已上线。
- `attach_lock` + `last_seq`：bridge 侧“接管 live 会话”的基础设施；Phase 4 detach 复用同一把锁，把 `kill_all=shutdown` 换成 `Detach` 语义即可。
- `OutputHub::attach` 的 checkpoint/replay：离线重放（Phase 4 场景）就是 “启动 GUI → attach 全部已知 live agent → 前端重建每个 tab”，`agent_resume` Attach-first 已走通该路径。
- `vt_checkpoint` + `hub.resize`：常驻跨尺寸（GUI 关/开期间 daemon 内 PTY 尺寸变化）重建正确，已有测试。
- 输入租约：Phase 4 需要“GUI 离开时释放、回来 attach 时重新取得”，server `cmd_attach` 的租约移交逻辑即复用点。
