# 期 2 — 连线 + 从根运行（task / exit code）

> **状态:** 已落地（连线 + 不可变 plan + 画内运行工具条；未引入 @xyflow/react，自研投影）
> **前置:** [phase-1b.md](phase-1b.md) 全部完成。
> **目标:** 证明图可执行：有向依赖、不可变 plan、exit code、失败挡住下游、反向依赖停止。
> **提交:** `feat(canvas): edges + immutable run plan`（单独 PR，不要和 1b 揉）
> **没有这一期，不要说「工作流」。**
> **不要做:** service/端口（期 3）、MCP、模板、卡片内 xterm、把 live 端口写进图。

顺序：Rust 领域（连线校验 + expand + plan）先绿，再换 React Flow 投影，最后接 Run IPC 和工具条。

---

## 步骤 1 — 加 `@xyflow/react`

```bash
pnpm add @xyflow/react
```

不要加 `@xyflow/react` 以外的画布库。`package.json` 锁定后 `pnpm tsc --noEmit` 应仍绿（尚未 import）。

**本步验收:** lockfile 更新；应用还能 `pnpm tauri dev` 起来（未引用新库）。

---

## 步骤 2 — Rust：禁环 + `expand_workflow`

**改:** `src-tauri/src/canvas_graph.rs`

纯函数（无 IPC）：

```rust
/// 有向、禁自连、禁环、端点必须是 terminals[].id。
pub fn validate_graph(graph: &BlockGraph) -> Result<(), String>;

/// 从 terminal_id 沿无向依赖可达的全部终端 id（完整流程）。
/// 未知 id → Err。Agent 控制台 id → Err。
pub fn expand_workflow(graph: &BlockGraph, terminal_id: &str) -> Result<Vec<String>, String>;

/// 对 expand 得到的子图做拓扑序。有环 → Err（validate 应已拦住）。
pub fn topo_order(graph: &BlockGraph, ids: &[String]) -> Result<Vec<String>, String>;
```

`validate_graph` 在期 1 已拒自连 / agent 端点。本期补：Kahn 或 DFS 检环；失败信息要可读（`cycle involving term_a`）。

`canvas_graph_set` 改为走完整 `validate_graph`（含环）。

可选新 command（前端连线用，避免每次 set 整图打架）：

```rust
#[tauri::command]
pub fn canvas_graph_connect(
    project: String,
    workspace_id: String,
    source: String,
    target: String,
) -> Result<BlockGraph, String>; // 返回提交后的图；失败原图不变
```

实现 = get → 加边 → validate → set → 返回新图。

**单测：**

| 测例 | 期望 |
| --- | --- |
| A→B→C，从 B expand | {A,B,C} |
| A 与 D 无边，从 A expand | {A} |
| A→B，B→A | set/connect Err，文件不变 |
| connect(agentId, term) | Err |
| connect(term, term) 自连 | Err |
| 两个分量，expand 不串台 | 只返回命中分量 |

```bash
cd src-tauri && cargo test canvas_graph
```

**本步验收:** 上表绿。

---

## 步骤 3 — React Flow 只投影

**改:** `CanvasPanel.tsx`（大改）、可能新建 `ui/components/canvas/canvasNodes.tsx`

- `ReactFlowProvider` 包一层
- `nodes` / `edges` 从 `BlockGraph` **派生**，`node.id` 用视图 id（如 `term:${id}` / `agent:${id}`），提交时 map 回领域 id
- 自定义 node types：`terminal`、`console`。复用 `CanvasNodeCard` 外观
- `onConnect`：`invoke("canvas_graph_connect", { project, workspaceId, source: domainId, target: domainId })`。失败 → `notify(t("canvas.connectFailed"), err)`，**不**乐观落边
- `onNodesChange` 里拖位置：仍 debounce `canvas_graph_set` 只更新 position/viewport
- `isValidConnection`：console 卡 / 自连在 UI 层灰掉，但 **不是** 事实来源；Rust 仍是唯一提交门
- Agent 控制台：`handles` 不渲染，或 handle 上 `isConnectable={false}`
- CSS transform / zoom：先 **不要** 往 node 里塞 xterm。本期继续 0 路 live 终端
- 手势：xyflow pan/zoom 取代自研相机。Todo 拖拽不要绑到 node。

i18n：`canvas.connectFailed` zh「无法连接」/ en「Can't connect」。

**本步验收:**

- 两张 shell 卡能拖出一条边，刷新后边还在
- 拖向 claude 控制台卡：连不上
- 自连：连不上
- 造环：第二条边失败，第一条还在
- 双击卡仍回 agent tab
- `tsc` 绿

---

## 步骤 4 — `canvas_run.rs`：不可变 plan（先内存、先单测）

**新建** `src-tauri/src/canvas_run.rs`。先不接 PTY。

```rust
pub struct RunPlan {
    pub run_id: String,
    pub project: String,
    pub workspace_id: String,
    pub terminal_ids: Vec<String>, // 拓扑序
    pub edges: Vec<(String, String)>,
    pub created_at: i64,
}

pub fn freeze_plan(graph: &BlockGraph, root_terminal_id: &str) -> Result<RunPlan, String> {
    let ids = expand_workflow(graph, root_terminal_id)?;
    let order = topo_order(graph, &ids)?;
    // edges 只保留两端都在 ids 里的
    // run_id = uuid
}

/// 跑期间再 freeze 一次应得到同一 terminal_ids 顺序（给定同一 graph 快照）。
```

把「开跑那一刻的 graph 克隆」存进 `Run`。之后 `canvas_graph_set` 改活图 **不** 改这份。

单测：

- A→B→C freeze 从 A：顺序 A 然后 B 然后 C（允许无依赖并行，但 A 必须在 B 前）
- freeze 后从 graph 删边再 freeze 活图：旧 RunPlan 不变
- root 是 agent 控制台 id → Err
- 鼠标 TUI runtime 出现在 terminal.kind/agent 绑定里 → 不要放进 plan（plan 节点只能是 `terminals`）

```bash
cd src-tauri && cargo test canvas_run
```

**本步验收:** 单测绿。尚未 spawn。

---

## 步骤 5 — Run IPC：start / status / stop

**改:** `canvas_run.rs` + `lib.rs` `generate_handler!`

内存表：`Mutex<HashMap<run_id, Run>>`。进程内即可（daemon 重启 Run 丢失可接受，契约未要求跨进程恢复 Run）。

```text
canvas_run_start { project, workspace_id, root_terminal_id }
  → freeze_plan
  → 每个 plan 节点：若 terminals[].agentId 已有 live/dormant session 则绑定它；
    否则记录「需要 spawn」（前端或 Rust 调现有 agent_spawn）
  → 无上游的节点进入 ready_to_start
  → 返回 { run_id, plan }

canvas_run_status { run_id }
  → { plan, nodeStates: { id: pending|running|ok|failed|blocked }, blocked: [] }

canvas_run_stop { run_id }
  → 按 plan 反向序 agent_kill（已启动的）
  → 从表里拆掉 Run
```

启动执行（建议 Rust 驱动，避免前端丢事件）：

1. ready 节点：若已有 agentId 且是 shell → `agent_write` 发送 `command`（空 command = 视为立刻成功？**不要**。空 command 的 task：绑定 session 后等 **下一次** exit 不合理。约定：`command` 非空才 write；空 command 的 task 在 start 时记 `ok`（no-op），方便测拓扑。）
2. 等 `agent://exited` / status `done|failed`（已有事件总线，搜 `agent://exited`）
3. exit 0 → 该节点 `ok`，解锁下游（所有直接上游都 ok）
4. 非 0 → 该节点 `failed`，所有下游（直接+间接）`blocked`，不 spawn
5. 禁止把鼠标 TUI session 当启动器：`is_shell_runtime` 的 Rust 等价（bash/shell/cmd/powershell）才 write；否则该节点 `failed` 并 blocked 下游

**不要**把 `port` / `listen` / `exitCode` 写进 `canvas_graph_set`。加一个单测：跑完后 `canvas_graph_get` 的 JSON 不含这些键。

`lib.rs` 注册三个 command。

**本步验收:**

- 单测：用假 graph + 假 exited 回调（把执行循环写成可注入 sink）覆盖 成功链 / 失败挡住 / stop 反向
- 若注入困难：先测 freeze + 状态机纯函数，PTY 接好后步骤 7 手动

---

## 步骤 6 — 画板内工具条（不是 TabBar）

**新建** `ui/components/canvas/CanvasToolbar.tsx`，挂在 `CanvasPanel` **内部**顶部，不要改 TabBar。

按钮：

- 运行：对 **选中的终端积木** 调 `canvas_run_start`。未选中 → disabled。选中控制台卡 → disabled，title `t("canvas.runNeedsTerminal")`
- 停止：有 active run_id 才亮
- 状态字：`t("canvas.runStatus", { state })`

i18n：`run` / `stop` / `runNeedsTerminal` / `runStatus` / `blocked` / `failed`。

节点外观：`nodeStates` 映射到卡片边框色（`--success` / `--danger` / `--warn`），不要新动画。

**本步验收:** 工具条在画布里，TabBar 右侧仍只有终端/画布切换。

---

## 步骤 7 — 手动端到端

准备：画布上两张 **shell** 卡 A、B。A 的 command 设成 `true`（或 `exit 0`），B 设成 `echo ok`。怎么改 command：本期最小可用 = graph JSON 里手改 `command` 字段，或卡片上一个只读 command + 以后再做编辑。若 1b 没 UI 编辑 command：**步骤 7a** 给终端卡加一个小输入（blur 即 `canvas_graph_set` 只改该 terminal.command）。单对象动作，不 expand。

手动：

1. A→B 连线。选中 A，点运行：A 先跑完，B 才出现 running。
2. 把 A 的 command 改成 `false` / `exit 1`，再跑：B 保持 blocked，不 spawn 新命令。
3. 跑的时候拖 B 的位置：启动顺序不变（B 仍等 A）。
4. 运行中点停止：B 若已起则先杀，再杀 A（看 pid / 侧栏状态）。
5. 造环：连不上。
6. 控制台卡当 root：运行按钮 disabled。
7. 画布上仍 0 个 xterm。
8. `canvas_graph_get` 原始 JSON 无 `port`/`listen`/`exitCode`。

---

## 步骤 8 — 安全与文档

- `docs/security-review.md` 补：`canvas_graph_set` / `canvas_run_*` 与 `agent_write` 同级
- `docs/CaPilot-IDE-RUNBOOK.md` 补 IPC 名
- `docs/canvas-view.md` 状态 → `期 2 已落地`
- 本文件顶部：`状态: 已落地`

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
```

---

## 完成清单（期 2 出门）

- [ ] A→B，从 A 运行：A exit 0 之后 B 才跑
- [ ] A 失败：B blocked
- [ ] 跑时拖节点：当前 plan 不变
- [ ] 停止：反向依赖杀进程
- [ ] 环 / 自连 / agent 端点：图不变
- [ ] expand / topo / freeze / 失败传播有单测
- [ ] 0 路 live xterm
- [ ] 运行按钮在 CanvasPanel 内，不在 TabBar

过了才能开 [phase-3.md](phase-3.md)。
