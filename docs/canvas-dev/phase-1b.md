# 期 1b — 投影卡片 + 位置持久化 + 双击回 tab

> **状态:** 已落地（2026-08-20）
> **前置:** [phase-1a.md](phase-1a.md) 全部完成（能切到空画布）。
> **目标:** 当前作用域每个 session 一张摘要卡。拖动位置写入图 JSON。双击 / Enter 激活已有 agent tab。节点里 **不** 嵌 xterm。
> **提交切分（两个 commit / PR 更好）：**
> 1. `feat(canvas): graph JSON IPC + persist positions`
> 2. `feat(canvas): project sessions as cards; double-click focuses agent tab`
> **不要做:** `@xyflow/react`、连线 UI、Run、卡片内 xterm、把端口写进图。

先做步骤 1–4（Rust IPC + 单测），再做步骤 5–9（前端投影）。Rust 没绿不要写前端 invoke。

---

## 步骤 1 — 冻结 graph JSON 形状

不写代码。把下面 schema 当契约 v1。实现时字段名必须一致（serde 默认 snake_case 的话，前端 invoke 用 camelCase——**选一种并在 struct 上 `#[serde(rename_all = "camelCase")]`**，与现有 `AgentUsage` 一致）。

路径：

```
<data_root>/workspaces/<project>/canvas/<workspaceHash>/graph.json
```

`workspaceHash`：对 `workspaceId` 做稳定短哈希（sha256 前 16 hex，或 URL-safe base64 截断）。**不要**把绝对路径当目录名。

```json
{
  "version": 1,
  "projectId": "foo",
  "workspaceId": "/abs/path/or/default-root",
  "viewport": { "x": 0, "y": 0, "zoom": 1 },
  "terminals": [
    {
      "id": "term_...",
      "name": "dev",
      "cwd": "/abs",
      "command": "",
      "kind": "task",
      "agentId": "<session id or null>",
      "position": { "x": 80, "y": 80 },
      "size": { "w": 240, "h": 88 }
    }
  ],
  "edges": [],
  "combinations": [],
  "agents": [
    {
      "id": "<agent session id>",
      "position": { "x": 400, "y": 80 },
      "size": { "w": 240, "h": 88 }
    }
  ]
}
```

规则（写进 `canvas_graph.rs` 文件头注释）：

| 项 | 规则 |
| --- | --- |
| `terminals[].agentId` | 引用，不是所有权。session 在 `sessions.db` |
| 投影 | `isShellRuntime` → 终端积木（`terminals`）；其它 runtime → `agents` 控制台 |
| `agents[].id` | **禁止**作为 `edges` 的 source/target |
| 空文件 | `get` 返回空图（空数组 + 默认 viewport），**不**写盘 |
| 幽灵 | `agentId` 指向已删 session：前端摘卡；`set` 时也可以滤掉 |
| 写盘时机 | 用户拖动 / 改 viewport 后才 `set` |

**本步验收:** 本段被贴进下一步的模块文档注释。无代码。

---

## 步骤 2 — `src-tauri/src/canvas_graph.rs` 骨架 + 路径安全

**新建** `src-tauri/src/canvas_graph.rs`。

需要从 `lib.rs` / `persistence` 复用：

- `crate::sanitize_project` — 现在是 `lib.rs` 的私有 `fn`。**这一步先把它改成 `pub(crate) fn sanitize_project`**，或在 `canvas_graph.rs` 里复制同规则（优先 pub(crate)，一处事实）。
- `crate::persistence::project_dir`
- `crate::persistence::path_is_within`（确认已有；git_gate 在用）
- worktree 列表：`Persistence` 上已有 worktree CRUD，get/set 的 `workspace_id` 必须等于该 project 的 `projectRoots` 等价路径（`custom_project_root` 或 `project_dir`）或一条 `WorktreeMeta.path`

建议命令签名（Tauri State 按现有 `setting_get` 抄）：

```rust
#[tauri::command]
pub fn canvas_graph_get(
    persistence: tauri::State<'_, Arc<Persistence>>,
    project: String,
    workspace_id: String,
) -> Result<BlockGraph, String>;

#[tauri::command]
pub fn canvas_graph_set(
    persistence: tauri::State<'_, Arc<Persistence>>,
    project: String,
    workspace_id: String,
    graph: BlockGraph,
) -> Result<(), String>;
```

若 `workspace_id` 校验只需要磁盘路径、不需要 DB：`persistence` 参数可先不用，但 project 仍必须 `sanitize_project`。

`graph_path`：

```rust
fn graph_path(project: &str, workspace_id: &str) -> Result<PathBuf, String> {
    sanitize_project(project)?;
    validate_workspace(project, workspace_id)?;
    let hash = workspace_hash(workspace_id);
    Ok(project_dir(project).join("canvas").join(hash).join("graph.json"))
}
```

`validate_workspace`：

1. `workspace_id` 含 `..` 或空 → Err
2. canonicalize（文件不存在时用 parent canonicalize + 文件名拼接，避免未创建 worktree 误杀）
3. 允许根 = `project_dir(project)`、`custom_project_root(project)`、该 project 登记的 worktree.path
4. `path_is_within(resolved, allowed_root)` 必须 true

`workspace_hash`：稳定、短、文件系统安全（`[0-9a-f]+`）。

**本步验收:** 模块能编译。下一步才注册 handler。先写空 `get` = 文件不存在则 `BlockGraph::empty()`；`set` = 未校验直接写也行，步骤 3 补校验。

---

## 步骤 3 — 读写、校验、原子写

实现：

**get**

1. `graph_path`
2. 文件不存在 → `BlockGraph::empty(project, workspace_id)`
3. 存在 → `serde_json::from_str`，`version != 1` → Err（不要静默升级）
4. 不要在 get 里写盘

**set**

1. `graph_path`，`create_dir_all(parent)`
2. `validate_graph(&graph)?`：
   - `version == 1`
   - 每个 edge：`source != target`；`source`/`target` 都在 `terminals[].id` 集合里；**不在** `agents[].id`
   - 期 1 允许 `edges: []` 以外的合法边存下来（给期 2 铺路），但环检测可留期 2。自连和 agent 端点 **现在就拒**
3. 失败 → 原文件不动
4. 写 `graph.json.tmp` 再 `rename` 成 `graph.json`

`BlockGraph::empty`：`version=1`，空 vec，`viewport: {0,0,1}`。

**本步验收:** 见步骤 4 单测。手工：`CAPILOT_HOME=/tmp/capilot-canvas-test` 跑一次 get（空）、set、再 get。

---

## 步骤 4 — 注册 IPC + Rust 单测

**改:** `src-tauri/src/lib.rs`

1. 顶部：`pub mod canvas_graph;`（或 `mod canvas_graph;`）
2. `generate_handler!`（约 4547）追加：

```rust
canvas_graph::canvas_graph_get,
canvas_graph::canvas_graph_set,
```

**单测**放 `canvas_graph.rs` 末尾 `#[cfg(test)]`，抄 `persistence.rs:1353`：temp dir + 设 `CAPILOT_HOME`（看 `data_root()` 是否读这个 env；是则测前 `std::env::set_var`）。注意测试并行会抢 env——用独立 subdir + 文件锁，或测 path 函数而不是走 `data_root()`。更稳：`graph_path` / `validate_graph` / 读写抽成不依赖全局 env 的纯函数，测这些。

至少覆盖：

| 测例 | 期望 |
| --- | --- |
| `get` 无文件 | 空图，不创建文件 |
| `set` 再 `get` | 位置 roundtrip |
| `project` 含 `..` | Err |
| `workspace_id` 含 `..` 或非允许根 | Err |
| edge 自连 | Err，原文件不变 |
| edge 的 source 是 `agents[].id` | Err，原文件不变 |
| 非法 JSON / version=2 | get Err |

```bash
cd src-tauri && cargo test canvas_graph
```

**本步验收:** 上表全绿。`pnpm tsc --noEmit` 仍绿（前端还没 invoke）。

---

## 步骤 5 — 投影 merge（前端纯函数）

**改:** `ui/state/canvas.ts`（或新 `ui/state/canvasGraph.ts`，避免 canvas.ts 膨胀）。

```ts
import { isShellRuntime } from "./shellPath";
import type { AgentInfo } from "./store";

export type BlockGraph = { /* 与 Rust camelCase 对齐 */ };

export function mergeAgentsIntoGraph(
  graph: BlockGraph,
  agents: AgentInfo[],
  scope: CanvasScope,
): BlockGraph {
  // 1. 过滤：agent.project === scope.projectId
  //    （workspace：agent.cwd 在 workspaceId 前缀下，或 workspace_id 匹配）
  // 2. shell → terminals（已有 agentId 的留下位置；没有的分配空位）
  // 3. 非 shell → agents[] 控制台
  // 4. 图里 agentId 已不在 agents 列表 → 摘掉
  // 5. 不 mutate 入参
}
```

空位算法：网格 `x = 80 + (i % 3) * 280`，`y = 80 + Math.floor(i / 3) * 120`。已占用位置跳过。

**本步验收:** 若不愿加测试框架，至少导出函数 + 在 `CanvasPanel` 用之前用 3 个手写断言（临时 `console.assert` 也可，提交前删）。推荐：`pnpm tsc --noEmit` 能过就算本步完，行为在步骤 9 验。

---

## 步骤 6 — `focusAgentTab`

**改:** `ui/state/canvas.ts`

从 `LeftSidebar.tsx:280-294` `openAgentTab` 抄出到共享函数（LeftSidebar 可改为调它，**可选**，不要为了抽而大重构）：

```ts
export function focusAgentTab(agentId: string): void {
  const s = useStore.getState();
  const agent = s.agents.get(agentId);
  if (!agent) return;
  if (!s.tabs.some((t) => t.id === agentId)) {
    s.addTab({
      id: agentId,
      type: "agent",
      agentId,
      title: agent.title || `agent-${agentId.slice(0, 6)}`,
    });
  } else {
    s.setActiveTab(agentId);
  }
}
```

agent tab 的 `id` 就是 session id（`spawnAgent` 如此）。

**本步验收:** `tsc` 过。

---

## 步骤 7 — `CanvasNodeCard`

**新建** `ui/components/canvas/CanvasNodeCard.tsx`

Props：`{ agent: AgentInfo; kind: "terminal" | "console"; selected; onSelect; onDoubleClick; onPointerDownDrag }`

内容：

- 标题：`agent.title`
- 副标：`<Icon name={runtimeIcon(agent.runtime)} size={12} />` + `effectiveAgentStatus(...)` 文案
- 状态颜色：复用 `.tab-status.st-*`（idle/running/…）
- `kind === "console"`：小标签 `t("canvas.consoleBadge")`（**步骤 7a 先加 i18n 键** zh「控制台」/ en「Console」），视觉弱一档（`opacity` 或更淡边）
- cwd 一行，`font-family: var(--mono)`，溢出省略
- **不要** import `XTermPanel`
- **不要** `draggable={true}`（HTML5 DnD）。拖走用 pointer 事件，由 Panel 处理
- **不是** todo drop target（不要 `acceptTodoDragOver`）

i18n 追加：

```ts
// zh canvas
consoleBadge: "控制台",
// en
consoleBadge: "Console",
```

CSS class 前缀 `.canvas-card`、`.canvas-card.selected`、`.canvas-card.console`。硬边、`border: 1px solid var(--rule2)`，跟 `.tab-item` 同一语言。

**本步验收:** `tsc` 过。`rg XTermPanel ui/components/canvas` 无命中。

---

## 步骤 8 — `CanvasPanel`：加载、相机、拖卡、保存

**改:** `ui/components/canvas/CanvasPanel.tsx`（替换 1a 占位，空图仍显示 emptyHint）

行为：

1. `useEffect`：`invoke<BlockGraph>("canvas_graph_get", { project: scope.projectId, workspaceId: scope.workspaceId })`。注意 Tauri 参数名：Rust 是 `workspace_id` 的话前端传 `workspaceId`（serde camelCase）或 `workspace_id`——**与步骤 2 的 rename_all 对齐，这里写死一种。**
2. `const agents = useStore(s => s.agents)` → `mergeAgentsIntoGraph`
3. 无卡：继续 emptyHint
4. 相机（自研，不要 xyflow）：
   - state：`viewport {x,y,zoom}`，初始来自 graph
   - 空白 `pointerdown` + move = pan
   - `ctrlKey || metaKey` + wheel = zoom（光标为锚，clamp 0.25–2）
   - 普通 wheel = 平移
   - 渲染：一个 `.canvas-world` 设 `transform: translate(x px, y px) scale(zoom)`；卡是它的 children，`position:absolute; left/top` 用 graph 坐标
5. 卡片 `pointerdown` `stopPropagation`，move 改该节点 `position`（坐标要除以 zoom、减 pan）
6. `pointerup` debounce 400ms 后 `canvas_graph_set`。payload = merge 后的 graph（含新 position / viewport）。**不要**写 Run 字段（本来也没有）
7. 单击选中；双击 / 选中后 Enter → `focusAgentTab(agentId)`
8. mousemove **不要** `useStore.setState`。用 `useState` / `useRef` 草稿，up 再 invoke
9. 不要 `data-tauri-drag-region`

切 scope：CanvasPanel 随 tab 卸载（key 已是 tab id）。**不要** `agent_kill`。

`worktree_remove`：Rust 侧成功删壳后 `fs::remove_dir_all(project_dir(name).join("canvas"))` best-effort。找不到 `worktree_remove` command 时，在 `lib.rs` 搜 `worktree_remove` 追加一行。失败只 log。

**本步验收:** `tsc` 过。见步骤 9。

---

## 步骤 9 — 手动 + 回归

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test canvas_graph
pnpm tauri dev
```

手动：

1. 项目里开 1 个 shell + 1 个 claude。切到画布：两张卡。shell 无「控制台」标；claude 有。
2. 拖开两张卡，关画布 tab，再打开：位置还在。
3. 确认磁盘：`$CAPILOT_HOME/workspaces/<project>/canvas/<hash>/graph.json`（dev 默认 `~/CaPilot/...`），**不是** localStorage。
4. 双击 claude 卡 → 激活该 agent tab，PTY 还是那个（输入还在）。
5. 侧栏关掉该 tab（不杀进程）再双击卡 → tab 被重新 `addTab`，session 恢复显示。
6. 杀掉 / 删除 session → 再开画布，卡消失，不留幽灵。
7. 新 spawn 一个终端 → 画布出现新卡（自动空位）。
8. `rg XTermPanel ui/components/canvas` 仍无命中。
9. 拖卡时 tab 条不跟着每像素重绘（目测）。
10. 1a 的切换按钮行为不回归。

---

## 步骤 10 — 文档

- `docs/canvas-view.md` 状态：`期 1a 已落地` → `期 1b 已落地`
- `persistence.rs` 文件头 layout 注释补 `workspaces/<project>/canvas/`
- 本文件顶部：`状态: 已落地`

---

## 完成清单（期 1b 出门）

- [ ] 当前项目每个 live/dormant session 有且仅有一张卡
- [ ] shell → 终端积木；claude/codex/dsh/pi → 控制台卡
- [ ] 拖卡刷新后位置还在（`data_root` 的 graph.json）
- [ ] 双击 → 对应 agent tab，同一 PTY
- [ ] `ui/components/canvas/` 无 `XTermPanel`
- [ ] `cargo test canvas_graph` 绿
- [ ] `pnpm tsc --noEmit` 绿
- [ ] 关 canvas tab 再开：viewport + 位置恢复
- [ ] 未宣称「有画布工作流」

过了才能开 [phase-2.md](phase-2.md)。
