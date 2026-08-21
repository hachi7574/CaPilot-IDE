# CaPilot 工作区画布视图 — 开发文档

> **日期:** 2026-08-20
> **状态:** 期 1a–3 已落地（TabBar 切换、卡片投影、连线/Run、端口租约）；期 4/5 领域不变量已进校验
> **分支:** `feature/canvas`
> **对照:** [cleancode](https://github.com/chen-985211/cleancode) 的 canvas-first 可执行工作区；语义细则见 [canvas-semantic-contract.md](canvas-semantic-contract.md)
> **实现步骤:** [canvas-dev/README.md](canvas-dev/README.md)（一期一个文件；主入口 = TabBar 右侧终端/画布切换）

CaPilot 保持 **多 harness 标签页 IDE**。画布是 **worktree 上的可执行终端图**，与编辑器 / Agent TUI 并存，不是把主界面改成流程图工具。

---

## 1. 结论

CaPilot **现在没有节点工作流画布**，只有标签页工作台 + HTML `<canvas>` 渲染（xterm、批注截图、CommitGraph 量字宽）。cleancode 才是 canvas-first：终端积木、依赖边、可执行 DAG。

CaPilot 要加画布：

- 做成 **工作区级新视图**（`Tab.type = "canvas"` 单例 tab）
- **复用**现有 PTY / worktree / agent adapter / `sessions.db`
- **不要** canvas-first，不要第一期把 live xterm 嵌进每个节点

---

## 2. 现状（CaPilot）

主界面：`TabBar` + `ContentArea` + `Composer`。`Tab` 只有 `agent | editor | diff | image`。

| 层 | 实际职责 |
| --- | --- |
| `ui/components/layout/ContentArea.tsx` | 按 tab 渲染 xterm / CodeMirror / 图片 / diff；可选 binary split tree |
| `XTermPanel` | xterm.js HTML canvas 画 PTY；mouse-TUI resident，切 tab 不卸实例 |
| `ui/state/annotations.ts` | 克隆 DOM、去掉 canvas 节点、再 `document.createElement("canvas")` 出图 |
| `CommitGraph.tsx` | SVG 图 + 隐藏 canvas 量文字宽度 |
| 工作区 | Zustand `worktrees[]` + Rust `worktree_*`；分支隔离已有，**没有图模型** |

技术栈：Tauri v2 + React 19 + Zustand + xterm。没有 `@xyflow/react`、没有节点图、没有依赖调度。

产品模型是「多 harness 标签页 IDE」：agent 常驻 PTY、Todo 拖进会话、文件树 + Git。和 cleancode 相反——cleancode 明确不做文件树和编辑器。

打开新 kind 的既有路径（image viewer，PR #2 / `328c1a117`）：扩 `Tab.type` → Panel 分支 → TabBar 图标/分组 → `addTab` helper。画布走同一条路，**不要**再造一套「画布 / 编辑器」主壳切换。

---

## 3. cleancode 画布在做什么

cleancode 是 canvas-first、local-first 可执行工作区：每个 Git worktree 一张画布，上面同时放 Coding Agent、终端任务、长驻服务和真实依赖边。

核心对象（产品契约，详见对照文档）：

- **终端**：最小执行单位（命令 + 任务/服务配置）
- **流程**：弱连通依赖分量，不可拆开跑；**没有**持久化流程 ID
- **组合**：可空、可持久的容器，不嵌套
- 命中流程中任一终端 → 扩展为整条流程（创建组合、快捷执行、右键同一规则）

架构拆开了事实来源（CaPilot 必须抄这一刀）：

| 上下文 | 拥有什么 |
| --- | --- |
| **BlockGraph** | 节点、边、viewport、组合、模板、快捷位 1–5 |
| **Run / WorkflowRun** | PTY 绑定、执行计划、就绪条件、端口租约、失败传播 |
| **AgentSession** | agent 身份与布局，**不是积木**，不参与 DAG |
| **CanvasArrangement** | 跨类型视觉堆叠（吸附/散开/网格）；不是组合、不是流程 |
| **Presentation** | React Flow 只投影，不改领域规则 |

运行语义：有向无环依赖；无上游可并行；任务看 exit code，服务看输出/TCP；上游失败挡住下游；停止按 **反向依赖** 杀进程。每次跑从当前图生成 **不可变执行计划**。Agent 通过 MCP 建图时，删节点 / 断依赖 / 跑停要人确认。

---

## 4. 两边差在哪

| | CaPilot IDE | cleancode |
| --- | --- | --- |
| 空间模型 | 标签 + 分屏 | 无限画布 + 节点 |
| 终端 | 一个 tab 一个 PTY | 一个积木一个 PTY，可连成 DAG |
| Agent | 主内容 | 画布上的控制台，和图分离 |
| 依赖 | 无 | 一等公民 |
| 工作区 | worktree 当项目 | 一分支一画布 + 执行作用域 |
| 编辑器 | 有 | 刻意没有 |

CaPilot 已有可复用资产：真实 PTY、resident xterm、worktree、多 runtime adapter、会话 SQLite。缺的是图聚合、工作流运行时、画布相机。

---

## 5. 产品边界

### 做

- 画布 = **当前 project / worktree 上一张默认图**
- 入口 = `Tab.type: "canvas"`，id `canvas:${projectId}`（有 worktree 则 `canvas:${worktreePath}`），单例打开
- 节点 v1 = **终端积木**（task / 以后 service）；卡片摘要，不是内嵌 TUI
- 双击 / 回车节点 → 激活已有 `agent` tab（复用 PTY）
- Agent 仍可在侧栏 / 普通 tab 开；画布上可摆 **控制台卡片**，**不进依赖边**
- 图与 Run 分开：端口分配、就绪、失败传播只活在某次 Run，不写回图 JSON

### 不做（非目标）

- 把 `ContentArea` 整页换成 React Flow，或「画布 / 编辑器」双主壳
- 让 UI 组件决定能不能连边（禁环、禁自连、流程扩展在领域层）
- 把 live 端口写进图
- 第一期嵌套子画布、CanvasArrangement 吸附/网格
- 第一期把完整 xterm 塞进每个节点
- 鼠标 TUI harness（claude / opencode / dsh 等）当「小卡片终端」
- 用 tldraw / Excalidraw 当主路径（自由绘制 ≠ 可执行工作区）

---

## 6. 架构

```
┌─ Presentation (React) ──────────────────────────────────────┐
│  CanvasPanel  (@xyflow 或自研 pan/zoom)                      │
│    节点卡 / 边 / 相机 / 选择                                  │
│  Zustand: selection, camera draft, drag preview only         │
└──────────────────────────┬──────────────────────────────────┘
                           │ invoke / events
┌─ Domain (Rust, 推荐) ────┴──────────────────────────────────┐
│  BlockGraph   节点、边、组合、viewport、身份                  │
│  Run          不可变 plan、绑定 agent_id、就绪、端口租约       │
│  Agent        现有 sessions；画布只引用 id + 控制台 layout    │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌─ 已有运行时 ─────────────┴──────────────────────────────────┐
│  PTY daemon / agent_runtime adapters / worktree_* / SQLite   │
└─────────────────────────────────────────────────────────────┘
```

**身份主键：** `projectId + workspaceId + objectKind + objectId`。不要用 React Flow `node.id` 当领域主键。

**持久化：** 图进 `data_root`（`sessions.db` 或 `workspaces/<name>/canvas-graph.json`），与 session/worktree 同命运。不要 localStorage。

**Worktree 切换：** scope 变了只卸画布投影；后台 PTY 按现有 daemon 策略存活。`worktree://removed` 才删图 + 杀该壳会话。

**高权限：** 图的写（删节点、改边、启动/停止流程）视同 `agent_write` / Git 级 IPC；MCP 改图必须确认。

---

## 7. 表现层要点

- 自定义节点：任务卡、服务卡（状态 / 地址）、agent 控制台卡（非积木）
- 节点默认摘要；**选中或双击**再挂 `XTermPanel`（沿用 resident；画布上最多 1 路 live 终端）
- CSS transform / 缩放时：`nopan`、停 xterm 交互，避免和 wheel / 指针抢事件（resident tab 已踩过 WebView backing store）
- 手势：画布空白拖 = 框选或平移，必须和 Composer / Todo / 路径 pointer 拖拽分区，不要 HTML5 DnD 混用
- 样式：LUCY 硬边、主题 CSS 变量、中英 i18n；图标走 `ui/assets/icons/`（禁止 emoji）

**库：** 若分期 2 很快要连线，直接 `@xyflow/react`；若第一期只投影卡片，自研 pan/zoom 更轻。不要为了「有画布」先上 tldraw。

---

## 8. UI 接入（对齐 image viewer）

1. `ui/state/store.ts`：`Tab.type` += `"canvas"`；可选 `projectId` / `worktreePath`
2. `ui/state/canvas.ts`（新）：`openCanvas(scope)` → `addTab({ id: \`canvas:${key}\`, type: "canvas", title })`
3. `ui/components/canvas/CanvasPanel.tsx` + 节点卡 + CSS
4. `ContentArea.tsx` `Panel` 增加 `tab.type === "canvas"` 分支
5. `TabBar.tsx`：图标、project 分组；画布 **不是** file-like，不进「关闭所有文件」
6. **主入口：TabBar 右侧切换按钮**（`.tab-add` 与窗口控件之间）在当前作用域的终端视角和画布视角之间切换；次入口：LeftSidebar 项目菜单「打开画布」
7. Ctrl+F 暂不进画布（与 editor/terminal 搜索分流）

Composer 仍挂在 MainArea 底部；发送逻辑继续认 `agent` tab，不认 canvas。

---

## 9. 运行层（接现有 PTY）

- 图 → 拓扑有序 **不可变** plan（开跑时冻结一份；跑期间拖节点不改这份 plan）
- 每个终端节点绑定现有 `agent_id` / bash session（`spawnBashAt` / `spawnAgent`）
- 任务：exit code；服务：输出正则或 TCP listen（后做）
- 端口：`fixed | preferred | auto`；分配结果只活在 Run
- 停流程：按反向依赖清理进程
- CaPilot 的 bash/shell runtime 比再包一层 PTY 更合适

鼠标 TUI agent **不要**当作 DAG 积木启动器。

---

## 10. 分期

| 期 | 交付 | 完成判据 |
| --- | --- | --- |
| **0** | 本文 + [语义契约](canvas-semantic-contract.md) | 规则有单一事实源 |
| **1a** | 空画布 + TabBar 切换 — [phase-1a.md](canvas-dev/phase-1a.md) | 能切到画布 |
| **1b** | 投影卡片 + 位置持久化 — [phase-1b.md](canvas-dev/phase-1b.md) | 空间化 session switcher，PTY 零嵌套 |
| **2** | 连线 + 从根运行 — [phase-2.md](canvas-dev/phase-2.md) | 证明图可执行；plan 不可变 |
| **3** | service 就绪 + 端口租约 — [phase-3.md](canvas-dev/phase-3.md) | Run 与图分离可验证 |
| **4** | Agent 控制台 + MCP（需确认）— [phase-4.md](canvas-dev/phase-4.md) | Agent owner 独立 |
| **5** | 模板 / 快捷 1–5 / Arrangement — [phase-5.md](canvas-dev/phase-5.md) | 最后做 |

「只有期 1」不要对外宣称「有画布工作流」。

---

## 11. 建议文件落点

```
docs/canvas-view.md                    # 本文
docs/canvas-semantic-contract.md       # 语义契约
docs/canvas-dev/                       # 一期一个实现步骤文件
ui/state/store.ts                      # Tab.type
ui/state/canvas.ts                     # openCanvas + UI 草稿
ui/components/canvas/CanvasPanel.tsx
ui/components/layout/ContentArea.tsx
ui/components/layout/TabBar.tsx
ui/components/layout/LeftSidebar.tsx
src-tauri/src/canvas_graph.rs          # BlockGraph 聚合（期 2 前可先 json）
src-tauri/src/lib.rs                   # canvas_graph_get/set, canvas_run_*
```

期 1 允许图先落 `workspaces/<name>/canvas-graph.json`，但读写仍走 Tauri command，不要让前端直接写盘。

---

## 12. 风险

- xterm 在 CSS transform 里：几何、wheel、WebView 丢 backing store（resident tab 已中招）
- 鼠标 TUI harness 不适合当小卡片终端
- 图编辑与 Composer / Todo / 路径拖拽手势冲突
- 执行计划必须不可变，否则 UI 拖节点会 concurrent 改正在跑的图
- WebKitGTK 热路径：画布重绘不能饿死 PTY 字节通道
- 图写 IPC 与 `agent_write` 同级，默认拒绝静默 MCP 改图

---

## 13. 一句话

CaPilot 保留 IDE；画布是 worktree 上的可执行终端图。PTY / worktree / resident xterm 留下。新建 BlockGraph（终端 + 边）+ Run（不可变计划 / 端口 / 就绪）+ 画布相机。Agent 是控制台不是积木。节点里终端后挂。React Flow 只投影。
