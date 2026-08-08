# Paseo 借鉴分析与实施方案

> 对标 `/home/hachi/Project/paseo`（Paseo v0.3.0-beta.2）源码，梳理可借鉴到 CaPilot 的能力、不建议照搬的部分，以及经确认的落地实施方案。**本文是设计文档，未动任何业务代码。**

---

## 给其他 AI 的交接（开工前先读这一节）

> 状态：**方案已定稿、决策已冻结，业务代码一行未动。** 本仓库是 CaPilot IDE（Tauri v2 Rust + React + xterm）。本文对标上级 `/home/hachi/Project/paseo` 给出可借鉴项与落地计划。若你的任务落在下表范围，按 **§5 实施顺序**推进；若在此之上还要动其他代码，先读根目录 `CLAUDE.md`（架构、命令、红线）。

### 本次要做的（已确认，按此顺序）
| 序 | 项 | 一句话 |
| --- | --- | --- |
| 0 | `build_on_exit` 接收 `Arc<Dispatcher>` | 让级联/失败回传闭环进 Dispatcher 方法 |
| 1 | ① 级联归档 + ③ busy 退出报失败 | Rust 核心，改动小收益直接 |
| 2 | ⑤ 前端终端合帧 | 独立、低风险，可并行 |
| 3 | ⑧ git 限流 + 白名单收口 | 新 `git_gate.rs`，性能+安全 |
| 4 | ⑥ workspaceId 三层分离 | 改动面最大，独立提交 |
| 5 | ② 子 agent 标签页解耦 | 前端，依赖 ⑥ |
| 6 | ④ attention 标记 + ESP 提醒 | 依赖 ①/③ 的信号，含 ESP |

### 硬约束（不可违反）
- **不加新的依赖**：限流用 tokio 自带 `Semaphore`，其余全部用现有栈（rusqlite、tauri、portable-pty、tokio）。
- **不装 agent hooks、不建 `~/.codex`/`.pi` 式目录**：身份只来自自有 sqlite，只读 `~/.claude/projects`。
- 保持既有机制（sqlite + PtyManager + zustand store），不引入新架构。
- 验证命令：`cargo test` **必须在 `src-tauri/` 内跑**；前端改动后 `pnpm tsc --noEmit`；动 IPC 面时两者都要过。
- 每步独立提交、先回归再合；不要在一步里混多个阶段。

### 关键代码位置（改前先读）
- 会话表/列迁移：`src-tauri/src/persistence.rs:189` `ensure_column`、`:213` CREATE TABLE、`AgentSessionRecord`。
- 自然退出分流：`src-tauri/src/lib.rs:135` `build_on_exit`（keep 分支 `:153` / delete 分支 `:179`）。
- worker 池：`src-tauri/src/orchestration/dispatcher.rs`（`report:418`、`sweep_stale_busy:219`、`resolve_worker:479`）。
- 手动删会话：`src-tauri/src/lib.rs:907` `sessions_delete`。
- git 命令群：`src-tauri/src/lib.rs:1353` 起。
- 前端终端写入：`ui/components/terminal/XTermPanel.tsx:208` `writeToTerm`、`:235` `fitAndRefresh`。
- 子标签页/事件：`ui/state/session.ts`、`ui/state/orchestration.ts`。
- ESP：`src-tauri/src/esp/{manager,protocol,ble,transport}.rs` + `ui/state/esp.ts`（协议中定义新消息类型的操作按 `esp/protocol.rs` 现状确认）。

### 未决（实现时才要决策，默认从简）
- ③ 失败回执是否需要"退出后 5s 内收到 report 则不算失败"的宽限期 → 默认不加，需要再与用户确认。
- ④ `agent://attention` 载荷格式与 ESP 侧 opcode → 实现时按 `esp/protocol.rs` 现有消息类型对齐。
- ⑥ 存量数据的回退路径保留旧启发式 `custom_project_root`，不做迁移。

最后一条红线别越界：**本阶段全部内容就是上面表格 0~6 项，不要顺手做 loop、relay、语音、移动端、SDK 接入**（见文中"不建议遵循"节）。

---

## 1. 背景：Paseo 是什么，和 CaPilot 什么关系

Paseo 是"用手机/网页/桌面/CLI 统一监控和指挥本地 AI 编码 agent"的桌面+移动应用，支持 Claude Code、Codex、Copilot、OpenCode、Pi。它跑一个本地 daemon（`packages/server`）管理 agent 进程、通过 WebSocket 把输出流给多个客户端，并有 agent 编排（子 agent、loop）、日程、语音、remote relay 等能力。

CaPilot 与它是**同赛道**：都围绕"管理本地 AI 编码 agent"展开。区别在形态——CaPilot 是本地 Tauri v2 桌面工作台（Rust + React + xterm），靠 PTY 直接驱动每条 CLI 的 TUI；Paseo 是 daemon + WebSocket + 移动/桌面客户端，靠 SDK/ACP 接入各 provider。两者在**编排、生命周期、状态机、数据身份、终端流、git 治理**上有大量可以互相借鉴的成熟设计。

以下借鉴点均来自 Paseo 的 `docs/architecture.md`、`docs/data-model.md`、`docs/agent-lifecycle.md`、`CLAUDE.md`，并落到 CaPilot 的真实代码位置。

---

## 2. 借鉴点逐条分析

### 2.1 子 agent 级联存档（值得抄，改动小）

**Paseo**：每个被父 agent 拉起的 agent 贴 `labels["paseo.parent-agent-id"]` 血缘标签；父 agent 归档时**递归级联**回收所有贴它标签的子 agent（`agent-lifecycle.md`："Cascade keeps subagent fleets from outliving their orchestrator"）。

**CaPilot 现状**：`dispatcher.rs` 里只有全局 `master_id: Mutex<Option<String>>`（`dispatcher.rs:76`），worker 不记录"属于哪个 master"。master 结束/删除后，派发出去的 worker 无依无靠，变成游离会话。

**价值**：整个工人舰队不会活得比调度者还久；主 agent 走人时 worker 自动收走，不残留孤儿终端。

### 2.2 "关标签页"与"归档"是两个概念（仅对子 agent 解耦）

**Paseo**（`agent-lifecycle.md` 的 Tabs vs Archive）：关**根 agent** 标签页 = 归档（它是这 agent 唯一的家，用户已习惯）；关**子 agent** 标签页 = 只关视图，agent 保留在父的"子轨道"，随时能再开。

**CaPilot 现状**：`session.ts` 关标签页 = 置 `done`，进侧边栏"已结束"组（`session.ts:90`）。worker 和根 agent 无差别。

**价值**：避免"随手关掉某个 worker 标签页"误伤它、要找回来费劲。子 agent 的"家"在 worker 轨道而非标签页。

### 2.3 完成信号契约 notifyOnFinish（关键缺口）

**Paseo**：创建子 agent 时带 `notifyOnFinish: true`（默认），子任务完成必须回报父 agent；fire-and-forget 才设 false。

**CaPilot 现状**：已有 `capilot report` shim → `dispatcher::report`（`dispatcher.rs:418`）做"回报→smart_return 分级→写回 master→worker 置 idle"，这是雏形。真正的缺口是**兜底判定**：worker 若没主动调 `report` 就退出了（崩溃/被误杀），现在只有 `sweep_stale_busy`（`dispatcher.rs:219`）3s 后把忙的 worker 置回 idle，**master 收不到任何告知**。

**价值**：把"完成判定"从"猜测式单向"变成"必须回传，否则报失败"。是后续 auto-attention、ESP 提醒的信号底座。

### 2.4 需要人工介入的标记（attention）

**Paseo**：agent 记录含 `requiresAttention` + `attentionReason`（`finished` | `error` | `permission`），UI 据此提醒。

**CaPilot 现状**：worker 只有 busy/idle/offline，无"需要人看一眼"字段。

**价值**：直接映射到 CaPilot 差异化——**ESP32 遥控**做"有 worker 需要处理"的震动/亮灯提醒；对"已完成/报错"精准提醒，而非只能看"跑没跑"。

### 2.5 终端流的合帧/水位线（命中 WebKitGTK 痛点）

**Paseo**（`architecture.md` WS 协议）：终端走独立二进制帧（opcode 区分 Output/Input/Resize/Snapshot）；输出侧 **4MiB 软背压、8MiB 硬水位线**，到阈值就合并大帧（coalesce）再发；resize **由"最后真正操作终端的人"决定**（只有尺寸真变/聚焦才发，被动渲染一律不发）。

**CaPilot 现状**：`XTermPanel.tsx:208` 的 `writeToTerm` 每次 `onmessage` 直接 `term.write`；`fitAndRefresh`（`:235`）每次 fit 都 `sendResize`。CaPilot 已在渲染纪律上做了优化（禁 CSS 光标闪烁、限幅 resize、不做反应式时间戳），但缺"多小包合并成一个大包"这一层。

**价值**：高频小包并成大块，减少 WebKitGTK 合成器重绘压力；纯前端局部改动，风险低。

### 2.6 项目 / 工作区 / agent 三层身份分离（根治 cwd 身份混淆）

**Paseo**（`data-model.md`）：`project`（人看，随机 `prj_id` + 名字 + root）、`workspace`（`wks_id` **opaque 随机，绝不当路径**，`cwd` 只作执行目录）、`agent`（挂 workspace）。"状态明明属于谁"问 workspaceId，**不靠 cwd 反推**。

**CaPilot 现状**：用 `workspaces/<name>/agents/<id>` 路径组织，custom-rooted（git clone/手动选文件夹）项目靠 `custom_project_root` 启发式回填（`persistence.rs:140`）+ `projectRoots` map 兜底。CLAUDE.md 自己标注："custom-rooted 项目需要 `projectRoots` map，不能靠 `workspaces/<name>` 匹配"——这是把"修补"当"正解"。

**价值**：消灭一整类身份 bug（同 cwd 多会话不再互相拖累、项目树稳定、删掉 map 兜底）。

### 2.7 store API 自管原子性

**Paseo**：store API 自己负责写入原子性；需要 queue / lock / 读-改-写循环的，逻辑应收进 store（`data-model.md` Store Surface 规则）。

**CaPilot 现状**：`sessions.db` 与 `.agent-meta.json` 双写（DB 权威）。任何绕过 `Persistence` 直接写 sqlite 的点都是隐患。

**价值**：审视收口，保持唯一权威，防双写不一致。

### 2.8 git 命令集中限流（两件事一条通道）

**Paseo**（`data-model.md` Git process limits）：所有 git 走 daemon 统一入口，`maxProcessesPerSecond:64`、`maxProcessConcurrency:8`。

**CaPilot 现状**：git 面板前端每 2.5s polling `git_*` 命令；`security-review.md` 把 `fs_*`/`git_*` 权限收紧列为预发布清单。

**价值**：一个收口（限流 + 路径白名单）同时服务性能（防 git 风暴）与安全（白名单只写一遍）。

### 2.9 协议卫生（低紧迫，留个概念）

**Paseo**：schema 只能加不能删、capability 门控、`// COMPAT(): added in vX, remove after <date>` 清理标签。

**CaPilot**：单一 Tauri 应用、前后端同发，紧迫性低。但值得记下两个概念：invoke 加过的字段别轻易删；未来若有第二个客户端（ESP 走协议 / CLI），直接照搬。

---

## 3. 不建议遵循的

- **relay / E2E 加密 / 语音 / 移动端 App**：Paseo 的差异化卖点（手机控制、云桥接），CaPilot 已有 ESP32 遥控作为自身差异化，方向不同。
- **用 Claude Code SDK 监听 task 协议**做子 agent 进度（Paseo 对 Claude 用 Anthropic SDK）：CaPilot 明确"不装 hooks、只读 `~/.claude/projects`、身份靠自有 sqlite"，这是自定原则，勿被带偏。

---

## 4. 实施方案（已确认）

### 范围确认

| 项 | 内容 | 血缘规则 | 完成判定 |
| --- | --- | --- | --- |
| ① | 级联存档 | **同项目回收**（master 结束收掉同 project 的非 done worker，不改 schema） | — |
| ③ | 完成反馈合同 | — | **busy 退出即报失败**（worker PTY 自然退出但 Busy = 视为未回传，合成失败报告给 master） |
| ⑤ | 前端终端合帧 | — | — |
| ⑧ | git 限流 + 白名单收口 | — | — |
| ⑥ | workspaceId 三层分离 | — | — |
| ② | 子 agent 标签页解耦 | — | — |
| ④ | attention 标记 + ESP 提醒 | — | — |

**不含**：loop 验收循环（另议）。

### 4.0 准备：`build_on_exit` 拿到 Dispatcher

`build_on_exit` 目前只捕获 `persistence + app`（`lib.rs:264`）。签名改为 `build_on_exit(persistence, dispatcher: Arc<Dispatcher>, app)`。Dispatcher 内部已持 `pty + persistence`（`dispatcher.rs:82`），级联与失败回传都做成 Dispatcher 方法，闭环在 orchestration 模块。

### 4.1 ① 级联归档 + ③ busy 退出报失败（Rust 核心，一起做）

新增 `Dispatcher` 两个方法：

```rust
// ① master 结束的级联
pub fn cascade_master(&self, master_id: &str, mode: CascadeMode) 
// mode = Keep(置done，可恢复) | Delete(连记录一并删)
// 规则：枚举同 project 的所有 role=worker 且非 done 会话
// 逐个：pty.kill(id) → update_status(done) → 双写 meta → emit agent://exited
//      → unregister_worker(id)（从池中移出，侧边栏"已结束"组仍可恢复）

// ③ busy worker 自然退出未回传 → 合成失败报告
pub fn worker_ended_naturally(&self, id: &str, exit_code: i32)
// 若该 worker Busy：构造 level=failure 的 WorkerReport
//   summary="worker 意外退出(exit=N)，未回传结果"
//   压入 reports + emit orchestration://report + 若 master PTY 活着则写回
//   最后 set_worker_idle(id)（同步置闲，不必等 3s sweep）
```

三个接线点（`lib.rs`）：
- `build_on_exit` **keep** 分支（`:153`）：对所有会话先 `worker_ended_naturally(agent_id, exit_code)`；再判断若 `role=master` → `cascade_master(id, Keep)`。
- `build_on_exit` **delete** 分支（`:179`）：若 master → `cascade_master(id, Delete)`（worker 连记录一起删）。
- `sessions_delete`（`:907`）：删 master 时同样 `cascade_master(id, Delete)`。

关键决策：级联用 `Keep`（置 done 可恢复）与既有约定一致；`Delete` 只在用户明确删 master / session_end_mode=delete 时触发。级联入口必须带日志（Delete 是破坏性操作）。

### 4.2 ⑤ 前端合帧（独立，先做无风险）

`ui/components/terminal/XTermPanel.tsx`：
- `writeToTerm`（`:208`）从"每次 onmessage 直接 term.write"改为**入队合并**：维护 `pendingChunks` + 调度中的 flush（`requestAnimationFrame` 或 ~33ms 定时器）；flush 时把 chunk 拼成一个 `Uint8Array` 一次性 `term.write`；`disposed` 时同步清空剩余。
- `attachChannel`（`:253`）与 store 缓冲路径（`:290`）不动（它们不经过写终端路径）。
- 顺带实现"被动渲染不发 resize"：`fitAndRefresh`（`:235`）只在尺寸真变/聚焦时发 resize。

### 4.3 ⑧ git 限流 + 白名单收口

- 新文件 `src-tauri/src/git_gate.rs`：`GitGate`（tokio Semaphore 并发 8 + 每秒启动上限 64）。
- `lib.rs` git 区（`:1353` 起）抽 `git_run(repo, args)`：路径白名单校验（repo 在 workspace_root / project root 内）+ 限流 + 执行；所有 `git_*` 改走它。
- 白名单只写这一处，同时服务性能与安全。

### 4.4 ⑥ workspaceId 身份分离

- `persistence.rs`：`ensure_column(sessions, "workspace_id", "workspace_id TEXT")`；`AgentSessionRecord` 加 `workspace_id: Option<String>`；`insert/get/list_all` 补列。
- `build_and_spawn` 落库时生成 `wks_<hex>`（不存在才生成）。
- `custom_project_root`（`persistence.rs:140`）：新数据用 `project.json` root + `workspace_id`；**保留旧启发式作回退**（不破坏存量）。
- 前端：归组键优先 `workspace_id`，缺失回退 `project+cwd`；`cwd` 仍只当执行目录。

### 4.5 ② 子 agent 标签页解耦（前端，依赖 ⑥）

- `ui/state`：worker 关标签页改为"只关视图"（新 `hideTab`），agent 保留在 worker 轨道；根 agent 行为不变。
- worker 轨道加显式"归档/删除"按钮（走 `sessions_delete`）+ "Archive finished"批量收起（学 Paseo `agent-lifecycle.md` 的 Archive finished）。

### 4.6 ④ attention + ESP 提醒（依赖 ①②③ 的信号）

- `persistence.rs`：`ensure_column` 加 `requires_attention INTEGER` + `attention_reason TEXT`；记录结构体同步。
- 置位点：
  - `worker_ended_naturally` 报失败 → 该 worker `attention_reason=error`；
  - 自然 done → `requires_attention` 时 `attention_reason=finished`；
  - （现有无 permission 事件，跳过该 reason）。
- 事件：Rust emit `agent://attention`（`{id, reason}`）；`ui/state/session.ts` 监听写 store（角标）。
- ESP：监控同一事件并转发紧凑载荷；`esp/manager.rs` 复用现有状态帧通路加 `attention` 消息。（`esp/protocol.rs` 里定 opcode——实现时确认当前消息类型。）

## 5. 实施顺序

```
①③ (Rust核心) → ⑤ (前端孤立) → ⑧ (git收口) → ⑥ (schema+模型) → ② (前端) → ④ (+ESP)
```

每步完成即 `cargo test`（`src-tauri/` 内）+ `pnpm tsc --noEmit`。

## 6. 验证

- `cargo test`：新增——persistence 的 workspace_id/attention 迁移、dispatcher 失败报告合成、git_gate 限流。
- `pnpm tsc --noEmit`。
- 夜间手工：master 派几个 worker → 验证 ① master 结束收掉 worker、③ worker 崩溃 master 收到失败回执、⑤ 刷屏不再卡 UI。

## 7. 风险

- ① 的 Delete 级联是破坏性的，只在明确删 master 时触发，入口带日志。
- ⑥ 改动面最大（schema + 前端归组），作独立提交、先回归再合。
- ③ 的失败回执文案克制（worker 正常自然结束但没调 report 也会被判失败）；是否需要"退出后短窗内 report 则不算失败"的宽限期，实现时再定，默认不加。

## 8. 决策记录

| 决策点 | 结论 |
| --- | --- |
| 借鉴落地范围 | ①②③⑤⑥⑧ + ④attention（含 ESP）；**不含** loop |
| 级联血缘规则 | 同项目回收（不加 `parent_id` 列） |
| 完成判定兜底 | busy 退出即报失败 |