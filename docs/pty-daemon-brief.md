# CaPilot PTY 守护进程改造 — 设计任务书（代码复核版）

> 本文以当前工作树中的 `src-tauri/src/agent_runtime/pty.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/persistence.rs`、`src-tauri/src/resource.rs` 以及前端 `ui/state/store.ts`、`ui/state/agentActions.ts`、`ui/components/layout/TabBar.tsx` 为准。文中的“现状”均对应具体实现，而不是目标行为。

## 0. 已确认的现状与目标

CaPilot 是 Tauri v2 桌面应用。当前由 GUI 进程内的 `PtyManager` 通过 `portable-pty` 管理每个 agent 的 PTY；锁定的 `portable-pty 0.9.0` 在 Unix 使用原生 PTY，在 Windows 使用 ConPTY。

当前生命周期语义如下：

- 应用收到 `RunEvent::ExitRequested` 时调用 `kill_all()`，所有 GUI 持有的 PTY 都会被终止。
- `agent_resume` 和 `agent_switch_runtime` 都先 `kill`，再以原 id、cwd 和 `resume_key` 启动新进程；它们现在不是对既有 PTY 的 attach。
- 活动会话点击 tab 的关闭按钮会走 `closeAgent` → `sessions_delete`，显式杀进程并删除 DB 行和 agent 元数据目录。Channel 发送失败导致杀进程是另一条异常清理路径，不能等同于“关闭 tab”。
- 前端在内存中最多保留每个 agent 约 2,000,000 字节的原始 PTY 输出；GUI 重启后这部分内容会丢失。
- 会话数据库是 `~/CaPilot/sessions.db`。当前 `SessionsDb::open` 没有启用 WAL，也没有为主会话库设置 `busy_timeout`；因此不能把“SQLite WAL 并发”当作已经具备的能力。
- `dormant` 主要是前端根据“没有 live Channel”推导出的显示状态；它不是后端的 PTY 保活/回收状态机。

目标是把 PTY 所有权移到独立、用户级守护进程，使 GUI 正常退出或崩溃后 agent 仍可运行，并在 GUI 重开后 attach 到同一个 PTY。这里的“跨重启”仅指 **GUI 重启**：守护进程崩溃或重启后，PTY master/ConPTY 句柄通常不可恢复，本设计不承诺继续 attach 原进程，而是安全地回退到 provider resume。

## 1. 约束与明确的产品语义

- 不引入 tmux/WezTerm 依赖。
- 保留前端 → Tauri command 的表面：`agent_spawn`、`agent_resume`、`agent_write`、`agent_resize`、`agent_kill`、`sessions_delete` 及 `on_data` Channel 仍可沿用。Tauri 后端内部改为守护进程客户端。
- 保留 `pty.rs` 已处理的竞态语义：子进程回收、阻塞写不持有全局锁、同 id 并发 spawn 的 token、陈旧 reader 的 generation 校验，以及显式 kill 对自然退出回调的抑制。迁移时应先用测试锁定行为，不能只复制代码后假定等价。
- GUI 正常退出、崩溃或 IPC 断开：只断开订阅，守护进程和 agent 保活。
- 关闭活动 tab：保持现状，仍走 `sessions_delete`，即杀进程并删除会话。若未来要提供“只关视图、后台继续运行”，应新增独立 UI 动作，不能悄悄改变 × 按钮语义。
- “休眠项目”：保持现状，显式杀掉该项目的 PTY，但保留可 resume 的会话记录。
- `agent_kill`、`sessions_delete` 和切换 runtime：保持显式终止/重启语义。
- `dormant` 继续表示“当前没有可 attach 的 live PTY、但会话可 resume”。守护进程内部可使用 `detached` 表示“PTY 仍活着但没有 GUI 订阅”，不要复用 `dormant` 以免改变 UI 含义。
- 守护进程是当前用户的后台进程，不安装为系统服务，不要求管理员权限。

## 2. 总体架构

```text
┌──────────────┐  Tauri Channel  ┌──────────────────┐  framed local IPC  ┌──────────────────┐
│ React/xterm  │ ◀──────────────▶ │ Tauri command    │ ◀────────────────▶ │ PTY daemon       │
│ (WebView)    │                  │ bridge (GUI 内)  │                    │ owns PtyManager   │
└──────────────┘                  └──────────────────┘                    └────────┬─────────┘
                                                                                │ portable-pty
                                                                                ▼
                                                                          ┌──────────┐
                                                                          │ agent PTY│
                                                                          └──────────┘
```

WebView 不直接连接 Unix socket 或 Windows named pipe。守护进程与 GUI Rust 后端之间使用本地 IPC；GUI bridge 再把字节转发到现有 Tauri Channel。因此“数据面迁出 GUI”不等于“移除前端 Channel”。

建议拆成三个边界：

1. **`pty_core`**：不依赖 Tauri，负责 spawn/write/resize/kill/reap、spawn token、generation 和进程信息。
2. **`OutputHub`**：接收 `pty_core` 的输出，维护序号、终端快照/增量日志和订阅者。订阅者失败只移除订阅者，绝不能触发 PTY kill。
3. **GUI bridge / daemon server**：分别负责 Tauri Channel 转发和本地 IPC。进程内回退模式可为 GUI bridge 配置现有的“Channel 发送失败则终止会话”策略，但该策略不能进入通用 `pty_core`。

若采用 trait，至少应表达为 `Send + Sync + 'static` 的输出事件接口，并返回可分类的订阅错误；不要把“致命 sink 错误”直接定义成子进程的致命错误。

## 3. 守护进程职责

- 独占管理所有 daemon 模式 PTY：spawn、attach、write、resize、kill、reap。
- 原子维护 live session 数量，计数必须包含 in-flight spawn。当前 GUI 侧 `live_count() >= 64` 后再 spawn 存在跨不同 agent id 的 TOCTOU；迁移时应以 slot reservation 修正，而不是原样下移。
- 保存每个 live PTY 的 generation、pid、尺寸、输出序号、终端快照和有界增量日志。
- 将自然退出持久化，并把 `agent://exited` / `agent://removed` 等生命周期事件写入可重放事件日志。GUI 在线时实时转发；GUI 离线后，下次连接按 sequence 补发并确认。
- 监测已有 status hook sidecar 的变化并去重记录。否则 GUI 离线期间发生的 `working → idle` 会丢失，todo 自动流转等现有上层逻辑无法补偿。
- 提供 PID 映射给资源采样器；资源采样可留在 GUI，也可移入守护进程，但只能有一个明确的数据源。
- 仅在用户明确请求停止守护进程、注销或系统关机时终止其 PTY。GUI 退出不是守护进程退出信号。

守护进程崩溃后必须把相关会话视为“PTY 状态未知/不可 attach”，不得根据旧 PID 文件声称仍可控制。daemon 应原子记录 PID、进程启动时间/平台等价 identity 和 generation；重启后只有在验证 identity 后清理了不可 attach 的旧进程，或确认它已经退出，才能返回 `NotLive`。若不能安全确认，阻止自动 respawn 并提示人工处理，避免同一 provider 会话出现两个 agent。下次 GUI 启动还应核对 instance lock、socket 和 daemon instance id。

## 4. IPC 与实例管理

### 4.1 传输与鉴权

- Unix（Linux/macOS）：Unix domain socket，父目录权限 `0700`、socket 权限 `0600`。
- Windows：当前用户 ACL 限定的 named pipe。
- 每次守护进程启动生成随机 token；GUI 通过只有当前用户可读的运行时文件取得 token，并在握手中提交。
- 使用 OS 级独占锁判定唯一实例。PID 文件仅用于诊断，不能单独作为互斥依据，因为会陈旧且存在 PID 复用。
- 握手必须包含 `protocol_version`、`daemon_instance_id`、应用版本和 capability 列表。协议不兼容（残留的上个构建的 resident daemon）由 GUI 桥自动替换：识别 `VersionMismatch` 后核验并终止旧进程、等待 socket 清空，再拉起同二进制的当前构建 daemon；始终不会静默启动第二套 PTY 管理器。
- 守护进程可执行文件必须作为 Tauri sidecar（`bundle.externalBin`）或同一可执行文件的明确 daemon 模式打包，并纳入 macOS 签名/公证、Windows 签名和 Linux 包验证；“新增 Cargo binary target”本身不会自动进入安装包。

### 4.2 帧与命令

协议使用有长度上限的二进制帧，至少携带 `request_id`、`agent_id`、`generation` 和消息类型。禁止依赖换行切 JSON，也不能让无上限帧耗尽守护进程内存。

控制面：

- `Spawn(args, initial_size)` → `AgentInfo`
- `Attach(id, initial_size, after_seq?)` → `AttachSnapshot`
- `Respawn(id, args, initial_size)` → `AgentInfo`
- `Write(id, generation, bytes)`
- `Resize(id, generation, rows, cols)`
- `Kill(id, generation?)`
- `List` → live session、pid、generation、last_seq、状态摘要
- `AckEvents(through_seq)`

数据/事件面：

- `Output { id, generation, seq_start, bytes }`
- `Exited { id, generation, exit_code, event_seq }`
- `Removed { id, event_seq }`
- `HookStatus { id, status, ts, event_seq }`
- 资源采样若放在守护进程，则增加 `ResourceSample`。

`Attach` 必须原子地完成“取得快照截止序号 + 注册后续订阅”，然后发送快照和 `seq > snapshot_seq` 的增量，避免 attach 窗口丢字节或重复。一个 agent 同时只允许一个输入控制租约；若未来支持多 GUI，其他连接默认只读。

### 4.3 断线与背压

Tauri `Channel::send` 的错误只能说明 WebView 转发失败（例如 WebView 已销毁或 `eval` 失败），当前 API 没有文档化的“饱和”分类。新架构不需要据此判断 agent 生死：

- GUI bridge 的 Channel 发送失败 → 取消该 GUI 的 daemon 订阅。
- daemon 到每个客户端使用有界发送队列；慢客户端超限时断开该客户端，并允许其重新 attach 获取新快照。
- 没有订阅者 → PTY 继续运行。
- 只有显式 `Kill` / `sessions_delete` / 项目休眠 / 守护进程显式关闭才终止 PTY。

## 5. 终端快照与缓冲边界

PTY 输出是有状态字节协议。按字节或“行数”截掉开头后直接重放并不可靠：截断点可能位于 UTF-8、CSI/OSC 转义序列中间，后续内容也可能依赖更早设置的 alt-screen、光标、颜色和终端 mode。“每次 attach 打起点标记”不会自动产生干净边界。

因此守护进程必须维护可恢复的 checkpoint，而不只是原始 ring buffer：

- 用 VT 解析器维护主屏、备用屏、scrollback、光标、样式和相关 mode。
- checkpoint 在完整解析边界生成，包含当时 rows/cols 和 `snapshot_seq`；其后保留有界原始增量。
- attach 时先对客户端终端做已定义的 reset，再发送可重建当前状态的 checkpoint，最后按 sequence 发送增量。
- attach 的 `initial_size` 在生成快照前生效；避免先按 24×80 回放、随后 resize 导致 TUI 残影。
- 对不支持完整序列化的 mode 建立兼容测试；不能仅依赖一次 resize pulse 作为“画面已恢复”的证明。
- 每会话和全局都设内存上限；超限时生成新 checkpoint 后才能丢弃旧增量。慢客户端有独立的有界队列。

快照只需在守护进程内存中跨 GUI 重启保留。若要跨守护进程重启保留历史，需要另行设计磁盘日志、加密/权限、版本兼容和清理策略，不属于本阶段目标。

## 6. 会话状态、持久化与离线事件

### 6.1 数据源与并发规则

`sessions.db` 是会话元数据的事实源，`.agent-meta.json` 是可修复的派生副本。daemon 与 GUI 进程共享持久层前，必须先补齐跨进程规则：

- 每个 SQLite connection 显式设置并验证 WAL，设置合理的 `busy_timeout`，写操作使用短事务并处理 `SQLITE_BUSY`。WAL 只解决 SQLite 并发，不解决 JSON 双写竞态。
- `.agent-meta.json` 更新需使用每 agent 的跨进程锁；锁内从 DB/最新文件重读，只修改目标字段，再以同目录临时文件 + 原子替换写入。不能让两个进程各自 read-modify-write 后互相覆盖 title/status/runtime。
- DB 写成功而 sidecar 写失败时，以 DB 为准；启动/attach 时执行幂等修复。删除也要在同一套锁与修复规则下处理。
- 自然退出、delete-mode 清理以及事件日志写入应使用可恢复的顺序；崩溃后通过 DB 行、live daemon snapshot 和事件日志对账。

可在共享模块中抽出 `SessionStore` 与 `LifecycleJournal`，供 GUI 回退模式和 daemon 模式共用。不能直接把带 `tauri::AppHandle` 的 `build_on_exit` 搬进 daemon；持久化事件与 Tauri emit 必须分层。

### 6.2 启动对账

GUI 连接后，以 `(daemon_instance_id, agent_id, generation)` 对账：

| DB 记录 | daemon live | 处理 |
| --- | --- | --- |
| 有 | 有 | 以 daemon 的 pid/generation 为 live 状态，可 attach |
| 有 | 无 | 标为无 live PTY；用户打开时走 provider resume |
| 无 | 有 | 视为孤儿；禁止重建同 id，记录诊断后显式终止或由既定恢复策略处理 |
| 无 | 无 | 无操作 |

`running` 这一持久化字段不能单独证明进程仍活着；实时状态必须结合 daemon snapshot。`detached` 是传输状态，不写成现有 `AgentStatus`。

### 6.3 resume / switch_runtime

- `agent_resume`：先 `Attach`；只有 daemon 已完成上述进程 identity 核验并明确返回 `NotLive` 时才 `Respawn`，并使用持久化 `resume_key`。连接超时、鉴权失败或协议错误不是 `NotLive`，不得据此 respawn，以免原 daemon 仍运行时制造重复 agent。为正确生成快照，可给现有 command 增加向后兼容的可选 rows/cols 参数，或采用先 resize 后 attach 的两阶段调用。
- `agent_switch_runtime`：先验证目标 runtime，再显式 `Respawn`，保持现有语义。失败恢复策略需定义，至少不能在验证前杀旧进程。
- `agent_spawn`：只创建新 id；daemon 内原子预留 live slot。

## 7. 资源采样与会话上限

当前 `resource::start_sampler` 每 3 秒从 GUI 内 `PtyManager::pids()` 获取根 PID。PTY 迁走后必须二选一并在实现前定稿：

1. **建议方案**：采样移入 daemon，结果经事件面推送；GUI 关闭时仍可维持连续的短期历史。
2. GUI 保留 `ResourceMonitor`，每 tick 先从 daemon `List` 获取 `(agent_id, pid, generation)` 再采样。

无论哪种方案，都要防 PID 复用：缓存和样本按 generation 失效。`MAX_LIVE_SESSIONS = 64` 在 daemon 内以原子 reservation 强制；进程内回退模式也复用同一 reservation 逻辑。respawn 替换已有 live slot，不应短暂计为两个永久名额，但新进程启动失败时必须释放 reservation。

## 8. 回退与升级安全

- 只有在启动阶段确认 instance lock 未被其他 daemon 持有、且 daemon 尚未接管任何会话时，才允许退回进程内 `PtyManager`。
- 已连接 daemon 后的瞬时断线应重连；不能“fail open”到进程内模式，因为原 daemon/agent 可能仍在运行。
- socket/pipe 被占用、token 不匹配或协议版本不兼容属于安全/升级错误，不是普通回退条件。
- 一次应用运行只选择一种 PTY owner。不要让“旧会话进程内、新会话 daemon”长期混跑；这会让 id、上限、资源采样和退出清理出现双重所有权。
- 更新时先握手判断版本。若必须更换 daemon，明确提示现有 PTY 将结束，并在关闭旧 daemon 后再启动新版本；本阶段不承诺跨 daemon 二进制热迁移 PTY 句柄。
- 回退模式保持当前行为：GUI 退出 `kill_all`，`agent_resume` 重新 spawn，Channel 发送失败按现有策略终止进程。

## 9. 迁移步骤

1. **行为固化与共享层抽取**
   - 为 Bug 1–5、并发上限 reservation、自然/显式退出、同 id kill+respawn 建回归测试。
   - 抽出无 Tauri 依赖的 `pty_core`、`OutputHub`、`SessionStore`、`LifecycleJournal`。
   - 为会话 DB 启用 WAL/`busy_timeout`，实现 sidecar 跨进程锁、原子替换和启动修复。
2. **daemon 与 GUI bridge**
   - 打包用户级 daemon，完成实例锁、权限、token、版本握手和有界帧协议。
   - 接入 spawn/write/resize/kill/list，确定资源采样归属。
   - 此阶段 GUI 退出仍可显式关闭 daemon，用于验证 IPC 与回退，不宣称跨 GUI 存活。
3. **attach 与可靠恢复**
   - 实现 VT checkpoint、输出 sequence、原子 snapshot+subscribe、背压断开与重连。
   - `agent_resume` 改为 Attach-first/NotLive-only-respawn；覆盖主屏、alt-screen、Unicode 分片和 resize 竞态测试。
4. **常驻与离线生命周期**
   - GUI 退出改为 detach；daemon 保活。
   - 守护进程负责自然退出持久化、status hook 事件记录及离线事件补发。
   - 完成 daemon 崩溃、应用升级、系统注销/关机和三平台安装包验证。

每一步都应保持进程内回退可编译、可测试，且不能在同一运行中产生两个 PTY owner。

## 10. 明确不做

- 不做多 pane shell 复用、配对 shell、远程访问。
- 不承诺 daemon 自身重启后继续 attach 原 PTY。
- 不改变 status hook 的文件格式和 agent 注入方式；daemon 模式只接管对 sidecar 变化的监测/记录，回退模式仍由 GUI 读取。
- 不把关闭 tab 改成隐式后台保活；若需要该能力，另做产品交互和清理策略。
- 不以 PID 文件、`status = running` 或一次 Channel/IPC 错误作为进程生死的唯一依据。

## 11. 验收标准

- `cargo test` 全绿；`pnpm build`（含 TypeScript）通过。
- 正常关闭 GUI 或强杀 GUI 后，daemon 和 agent 保持存活；重开后 attach 到相同 `(daemon_instance_id, agent_id, generation, pid)`，不调用 provider resume。
- 活动 tab 的 ×、`sessions_delete` 和项目休眠仍分别保持现有 kill/delete 或 kill/keep-record 语义。
- attach 在持续输出期间无丢字节、无重复；主屏、alt-screen、UTF-8/转义序列跨 chunk、不同尺寸下都能重建，且不会把截断的原始 ring buffer 当作完整历史。
- daemon 崩溃后不会重复控制或重复 spawn；验证并清理旧进程 identity（或确认其已退出）且返回 `NotLive` 后，才能按 `resume_key` 恢复。无法安全核验时阻止自动恢复并给用户明确状态。
- 两个独立进程并发读写 `sessions.db` 和同一 `.agent-meta.json` 的压力测试通过，无 `SQLITE_BUSY` 泄漏、字段回退或损坏；启动修复可收敛到 DB 事实源。
- GUI 离线期间的自然退出、delete-mode 和 hook 状态变化在重连后可重放；todo 等依赖状态迁移的逻辑不会漏事件。
- daemon 模式下资源面板仍约每 3 秒更新；PID generation 切换后旧历史不串到新进程。
- 64 会话上限在并发 spawn 压测下仍严格成立，失败 spawn 会释放名额。
- daemon 不可启动且确认没有其他 owner 时自动回退进程内模式；已存在 owner 或鉴权失败时不会静默回退。协议不兼容不会回退，而是替换：终止残留的旧构建 daemon 后用当前构建拉起新 daemon（§4.1）。
- Linux、macOS、Windows 的打包产物都包含并能启动已签名/受权限约束的 daemon，socket/pipe 仅当前用户可访问。
