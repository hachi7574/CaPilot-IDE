# 画布语义契约（CaPilot）

> **日期:** 2026-08-20
> **状态:** 设计稿（尚未实现）
> **对照:** [cleancode canvas-semantic-contract](https://github.com/chen-985211/cleancode/blob/main/docs/product/canvas-semantic-contract.md)
> **产品入口:** [canvas-view.md](canvas-view.md)
> **实现步骤:** [canvas-dev/README.md](canvas-dev/README.md)

本文是 CaPilot 画布上 **终端、流程、组合、Agent 控制台、Run** 的唯一产品事实来源。UI、MCP、模板、侧栏不得各自重新定义这些对象。架构与 IPC 以 [CaPilot-IDE-RUNBOOK.md](CaPilot-IDE-RUNBOOK.md) 为准。

结构化契约版本：`1`（实现前冻结；破坏性变更必须 bump）。

---

## 1. 统一语言

| 对象 | 定义 |
| --- | --- |
| **终端积木** | 最小执行单位。绑定一条 CaPilot shell/bash 会话（PTY）。`kind = task \| service`。 |
| **流程** | 由依赖连线形成的完整弱连通终端集合，是不可拆分的执行整体。**没有**持久化流程 ID。 |
| **顶层执行单元** | 一个独立终端积木，或一条完整流程。 |
| **组合** | 可持久存在的独立容器与连线作用域，可含 0 个或多个完整执行单元。不嵌套。 |
| **Agent 控制台** | 已有 Coding Agent 会话在画布上的投影（身份 + 布局）。**不是积木**，不出现在依赖边上。 |
| **图 (BlockGraph)** | 已提交的终端、边、组合、viewport。领域主键见 §3。 |
| **Run** | 一次执行：不可变 plan、PTY 绑定、就绪、端口租约、失败传播。与图分离。 |
| **完整流程扩展** | 从任意已命中终端沿 **无向** 依赖可达关系取得整条流程。 |
| **作用域** | `projectId + workspaceId`（workspaceId = 默认项目壳或 worktree path）。一张作用域一张图。 |

单击、查看、配置、聚焦 PTY 等 **单对象动作** 只作用于当前积木或当前 Agent 控制台，不做流程扩展。

---

## 2. 结构化规则

对同一组终端与依赖，所有消费者必须得到相同结果：

1. 一个无依赖终端识别为终端。
2. 一个包含两个或以上终端的弱连通依赖分量识别为流程。
3. 两个或以上顶层执行单元识别为可创建组合的集合；用户也可显式创建空组合，或让组合只容纳一个终端 / 一条流程。
4. 空组合持续存在，直到用户显式解散或删除。
5. 创建、加入或移出组合成员时，命中流程中的任意终端都扩展为完整流程；一次动作的扩展、占用校验和提交必须原子完成。
6. 同一终端不能同时属于两个组合。
7. 边：有向、禁自连、禁环。UI 不能在校验失败时画出「临时合法」的已提交边。
8. Agent 控制台可以放在画布上、可以进 **CanvasArrangement 堆叠**（后置功能），但 **不能** 成为边的端点，也不能成为组合成员里的「执行单元」。
9. 快捷执行（后置）：无依赖终端保存终端 ID；流程保存绑定时完整流程的精确终端 ID 集合；组合保存稳定组合 ID。流程不获得新的持久化身份。

模板、MCP、右键菜单、工具栏必须复用同一分类，不得各写阈值。

---

## 3. 身份与所有权

领域主键：

```text
{ projectId, workspaceId, objectKind, objectId }
objectKind = terminal | combination | agent
```

- React Flow / DOM node id **不是**主键；投影层可有自己的视图 id，提交时必须映射回领域 id。
- **BlockGraph** 拥有终端、边、组合、viewport 的已提交事实。
- **Agent 会话表**（现有 `sessions.db` / `.agent-meta.json`）拥有 agent 身份、runtime、cwd；画布只存引用 + 控制台 layout。
- **Run** 拥有某次执行的 plan、端口租约、就绪光标、失败原因。
- **CanvasArrangement**（后置）只拥有视觉堆叠关系，不改变依赖、组合或 Run。
- **Presentation** 只投影；`canConnect` / `canCreateCombination` 的结果来自领域，不来自组件。

图写失败时保持原图不变。跨 owner 操作（先挪节点再写堆叠）失败必须补偿。

---

## 4. 图 vs Run

| 写进图 | 只活在 Run |
| --- | --- |
| 终端 id、name、cwd、command、kind、size、position | 本次分配的端口、实际 listen 地址 |
| 边 source → target | 拓扑冻结后的 immutable plan |
| 组合成员 | 每个积木绑定的 `agent_id`（本次） |
| viewport | 就绪探测状态、exit code、失败挡住的下游集合 |

规则：

- **开跑时**从当前图生成一份不可变 plan；跑期间拖节点、改边 **不** 改这份 plan。
- 端口策略 `fixed | preferred | auto` 可以写在终端配置里；**分配结果**不写回图。
- 停流程：按 plan **反向依赖** 清理进程，再拆 Run。
- 任务成功/失败看 exit code；服务就绪看输出正则或 TCP listen（期 3）。
- 无上游的积木可并行；上游失败挡住下游，不盲启。

---

## 5. PTY 绑定

- 终端积木绑定 CaPilot 已有 **shell/bash** 会话（`spawnBashAt` / shell runtime），不是再包一层 PTY。
- 节点默认渲染摘要卡。选中或双击再挂 `XTermPanel`；画布上同时最多 **1** 路 live xterm。
- 鼠标 TUI agent（claude / opencode / dsh / pi 等）**不当**作 DAG 积木，也不在卡片内嵌完整 TUI。
- Resident 策略继续只服务 agent **tab**，不服务画布缩略卡。

---

## 6. 消费者投影

| 消费者 | 复用方式 |
| --- | --- |
| BlockGraph | 完整流程扩展、成员占用、禁环，提交前校验 |
| CanvasPanel / xyflow | 只渲染已提交图 + UI 草稿；连线 drop 走 invoke 校验 |
| Tab / 侧栏 | 双击积木 → 已有 agent tab；不在画布里再造一套会话列表真相 |
| Worktree | 切 scope 卸投影、保留后台进程；remove worktree 才删图 |
| Composer / Todo 拖拽 | 单对象动作；不把拖入当成「加入流程」除非走组合 API |
| MCP（后置） | 同一分类；改图 / 跑停需确认；不能绕过 BlockGraph |

---

## 7. 非目标

- 不引入持久化流程实体或流程 ID
- 不把 CaPilot 改成 canvas-first（编辑器、文件树、Git、Composer 保留）
- 不规定菜单视觉、定位、动效
- 不让 Agent、MCP 或 React 组件成为组合/边有效性的事实来源
- 不把 live 端口、exit code、日志写进图 JSON
- 契约 v1 不规定 CanvasArrangement 的吸附几何（见 cleancode 文档，后置）

---

## 8. 验证不变量（实现时变成测试）

- 单个完整流程的分类是 `workflow`，不能把其中一截终端单独拉进另一个组合。
- 组合成员动作从流程任意终端得到相同的完整成员集合。
- 边校验失败则图不变。
- 开跑后改图不影响当前 Run 的 plan。
- 端口租约不出现在 `canvas-graph` 持久化快照里。
- Agent 控制台 id 不能作为边的 source/target。
- 同一 `canvas:${scope}` tab 单例；关应用后图仍在 `data_root`，tab 本身与 editor 一样可不恢复（产品可改为恢复，但图文件必须还在）。

---

## 9. 一句话

终端积木组成图；流程是连通结果不是实体；Agent 是控制台；Run 是一次性不可变计划。UI 只投影。
