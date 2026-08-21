# 期 4 — Agent 控制台摆放 + MCP 建图（需确认）

> **状态:** 部分落地（控制台卡投影 + 禁边/禁组合已在领域层；MCP 确认未做）
> **前置:** [phase-1b.md](phase-1b.md) 至少完成（控制台卡已投影）。期 2 连线规则必须已拒 agent 端点。期 3 非必须。
> **目标:** Agent 控制台是画布上的一等**视觉**对象，但永远不是积木；MCP 改图 / 跑停必须经过确认。
> **提交切分：**
> 1. `feat(canvas): agent console layout as first-class projection`（若 1b 已够，本 commit 只补缺口）
> 2. `feat(canvas): MCP graph edits require confirmation`（可独立、可更晚）
> **不要做:** 控制台进边、进组合成员执行单元、静默 MCP 写盘、卡片内嵌 TUI。

期 1b 已把非 shell session 画成控制台卡。本期补齐「owner 独立」和 MCP 门闩。

---

## 步骤 1 — 缺口审计（先列后改）

对照契约 §2.8 / §3 / §6，检查：

| 不变量 | 1b/2 是否已有 | 没有就在本步补 |
| --- | --- | --- |
| 控制台 id 不能当 edge 端点 | 期 2 `validate_graph` | 补单测若缺 |
| 控制台不能进 `expand_workflow` | 期 2 | 补 |
| 控制台可拖、位置在 `agents[]` | 期 1b | 补 set 时不要把控制台写进 `terminals` |
| 双击回 agent tab | 期 1b `focusAgentTab` | 补 |
| 切 scope 不杀 PTY | 1a/1b | 回归 |
| 组合成员（若已做组合）不含控制台 | 期 5 才有组合；若提前做了组合，这里挡 | |

**本步验收:** 书面 checklist 全绿或已开 ticket 到对应步骤。

---

## 步骤 2 — 控制台卡交互打磨

- 选中控制台：右侧或 hover 不出现「运行」（期 2 工具条已 disabled，确认）
- 右键菜单（画布，不是 TabBar）：仅「打开终端」= `focusAgentTab`、可选「从画布隐藏」（从图 `agents[]` 删掉 **引用**，不 `agent_kill`）。隐藏后新投影策略：被用户隐藏的 id 记在 graph `agentsHidden?: string[]`，merge 时跳过。不要用 localStorage。
- 不允许从控制台拖出 handle。

i18n：`hideFromCanvas` / `openTerminal`（1a 已有 `canvas.openTerminal`）。

**本步验收:** 隐藏控制台 ≠ 结束会话。侧栏还在。刷新画布仍隐藏，直到用户「显示全部控制台」或删 `agentsHidden`。

---

## 步骤 3 — MCP 工具面（可推迟）

若本期不做 MCP：在本文件顶部标 `MCP: 未做`，步骤 3–5 跳过。**不要**先暴露写工具再补确认。

若做，工具必须走已有 `canvas_graph_*` / `canvas_run_*`，禁止新的「直接写 graph.json」工具。

建议工具（名字可调）：

- `canvas_list`（读）
- `canvas_add_terminal` / `canvas_connect` / `canvas_disconnect` / `canvas_remove`（写）
- `canvas_run` / `canvas_stop`（跑停）

读工具可自动。写 / 跑停：**默认拒绝**，直到确认。

**本步验收:** 没有确认通道就不要注册写工具。

---

## 步骤 4 — 确认 UI

复用 `PermissionConfirmationDialog` 或同视觉的对话框。

前端在 MCP 写请求到达时（桥接层：搜现有 MCP / permission 怎么进 Composer）：

1. 列出将要发生的事：删哪些节点、断哪些边、是否 start/stop Run
2. 用户确认 → 才 `invoke("canvas_graph_set" | "canvas_run_start" | ...)`
3. 取消 → 原图 / 无 Run

Rust 侧即使前端被绕过：没有「MCP 专用后门 command」。MCP 也只能调同一组 `canvas_*`。若 MCP 跑在 agent 进程里通过 `agent_write` 乱来——那是 PTY 里的 CLI，不是 CaPilot 图；图只认 IPC。

**本步验收:** 自动化：伪造一次 connect 请求，不点确认则 `graph.json` 不变。手动：点确认后边出现。

---

## 步骤 5 — 文档与安全

- `docs/security-review.md`：MCP 改图需确认；无静默写
- `docs/ai-runtime-references.md` 仅当某个 runtime 真接了 MCP 建图时才记
- `canvas-view.md` 状态 → `期 4 已落地`（若 MCP 未做写 `期 4 控制台已落地 / MCP 未做`）

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
```

---

## 完成清单（期 4 出门）

- [ ] 控制台可摆、可藏、可双击；藏 ≠ kill
- [ ] 控制台仍不能连边、不能当 run root
- [ ] MCP 写工具要么没有，要么必须确认
- [ ] 无第二条写盘路径
