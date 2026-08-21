# 期 5 — 模板 / 快捷 1–5 / CanvasArrangement

> **状态:** 部分落地（组合成员互斥已在 `validate_graph`；模板 / 快捷槽 / Arrangement UI 未做）
> **前置:** [phase-2.md](phase-2.md) 必须完成。未做期 2 **不要做本期**。
> **目标:** 可复用的终端/流程/组合模板；快捷槽 1–5；可选的视觉堆叠（吸附/网格）。不改变依赖语义。
> **提交:** 可再拆三个 PR（模板、快捷、Arrangement）。
> **不要做:** 持久化流程 ID；嵌套组合；让 Arrangement 变成组合；自由绘制工具（tldraw）。

契约：流程没有自己的 id。快捷槽存的是「绑定时的终端 id 集合」或「组合 id」或「单终端 id」。

---

## 步骤 1 — 组合（若还没有）

契约 §2：组合是可空、不嵌套的容器，成员动作命中流程中任一终端都 expand 为整条流程，且原子提交。

Rust：

```rust
pub struct Combination {
    pub id: String, // 稳定，可进快捷槽
    pub member_terminal_ids: Vec<String>,
}

pub fn can_create_combination(graph: &BlockGraph, hit_ids: &[String]) -> Result<Vec<String>, String>;
// 内部：对每个 hit expand_workflow，并集，检查没有终端已属其它组合
```

`canvas_graph_set` 校验：同一 terminal 不能出现在两个 combination；成员必须是 terminals；agent 控制台禁止。

UI：框选多个卡 → 工具条「编组」。解散 = 从图删 combination，终端留下。

单测：从流程中间一节点编组 = 整条流程进组；截一截进另一组 → Err。

**本步验收:** `cargo test` 组合不变量（契约 §8 前三条）。

---

## 步骤 2 — 模板

存哪：`<data_root>/workspaces/<project>/canvas/templates/<id>.json` 或全局 `<data_root>/canvas-templates/`。走 Tauri command `canvas_template_save` / `list` / `instantiate`。instantiate = 深拷贝节点（**新** terminal id）+ 边，position 偏移，写入当前图。

模板种类（契约 §2.9）：

- 单终端 → 存 terminal 配置（command/kind/portPolicy），不含 agentId
- 流程 → 存绑定时精确 terminal **配置**集合 + 边（不是 id 引用活 session）
- 组合 → 存组合结构

**不要**把 Run / 端口 / agentId 放进模板。

**本步验收:** 存一个 A→B 流程模板，在空画布实例化得到两个新 id 的积木和一条边，没有旧 agentId。

---

## 步骤 3 — 快捷槽 1–5

图或独立 KV（不要滥用 `setting_set` 除非加 allow-list 键）：推荐写在 graph 旁 `shortcuts.json`：

```json
{ "1": { "kind": "terminal", "id": "term_..." },
  "2": { "kind": "workflow", "terminalIds": ["t1","t2"] },
  "3": { "kind": "combination", "id": "comb_..." } }
```

workflow 槽存 **绑定时** 的精确 id 集合。之后图若增节点，快捷仍只跑那一批——这是契约。若集合不再是单一弱连通分量：运行时 expand 各自的分量还是拒绝？**拒绝并提示**（更安全）。

UI：CanvasToolbar 五个槽；键盘 `1`–`5` 在画布聚焦时 start run。输入框 / Composer 聚焦时不要抢。

**本步验收:** 绑 2 到一条流程；改图加第三节点；按 2 仍只跑原来两个（或按上面「不再连通则拒绝」——实现选拒绝的话测拒绝）。文档写死所选行为。

---

## 步骤 4 — CanvasArrangement（可选、最后）

视觉堆叠：吸附 / 散开 / 网格。只改 position，不改边、不改组合、不改 Run。

可自研；仍不要 tldraw。

契约 v1 不规定吸附几何——本步自己定常数（例如 8px 网格）并写进代码注释。

**本步验收:** 开跑后 Arrangement 散开：plan 顺序不变；边还在。

---

## 步骤 5 — 文档

- `canvas-view.md` 状态 → `期 5 已落地`
- 快捷槽语义写进契约附录（若与 §2.9 有选择点）
- 本文件顶部：`状态: 已落地`

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
```

---

## 完成清单（期 5 出门）

- [ ] 组合不嵌套、不截流程、不收控制台
- [ ] 模板实例化出新 id、无 agentId / 无端口
- [ ] 快捷 1–5 按契约绑定；Composer 聚焦时不触发
- [ ] Arrangement 只动位置
- [ ] 仍无持久化流程实体
