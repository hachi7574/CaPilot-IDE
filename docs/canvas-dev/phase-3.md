# 期 3 — service 就绪 + 端口租约

> **状态:** 已落地（Run 内端口租约 + `{PORT}` 注入；分配结果不写回图）
> **前置:** [phase-2.md](phase-2.md) 全部完成（task / exit code 链可跑）。
> **目标:** `kind: service` 的积木按就绪条件放行下游；端口 `fixed | preferred | auto` 的**分配结果**只活在 Run，不写回图。
> **提交:** `feat(canvas): service readiness + port leases`
> **不要做:** MCP、模板、CanvasArrangement、把 listen 地址写进 graph.json。

---

## 步骤 1 — 图字段（配置，不是租约）

终端积木可多这些 **图内** 字段（仍属 BlockGraph）：

```ts
kind: "task" | "service";
portPolicy?: "fixed" | "preferred" | "auto"; // 默认 auto
port?: number;           // fixed / preferred 的申请值；auto 则缺省
readyPattern?: string;   // 输出正则；空则只用 TCP
```

**禁止**上图的字段：`listenAddress`、`allocatedPort`、`leaseId`、`exitCode`、`ready`。

`validate_graph`：`kind=service` 允许无 command；`portPolicy=fixed` 必须有 `port` 且在 1–65535。

单测：set 一张带 `allocatedPort` 的 JSON → **要么 serde 忽略未知字段且 get 不回传，要么 validate 拒**。推荐 `#[serde(deny_unknown_fields)]` 太严（向前加字段会疼）；改为 get 序列化白名单，测试断言 roundtrip 不含 `allocatedPort`。

**本步验收:** `cargo test canvas_graph`；手写 JSON 塞 `allocatedPort`，get 回来没有它。

---

## 步骤 2 — Run 侧租约表

**改:** `canvas_run.rs`

```rust
pub struct PortLease {
    pub terminal_id: String,
    pub allocated: u16,
    pub listen: String, // 如 127.0.0.1:allocated
}

pub struct Run {
    pub plan: RunPlan,
    pub leases: HashMap<String, PortLease>,
    // ...
}
```

分配：

| policy | 行为 |
| --- | --- |
| `fixed` | 占用 `port`；已被本 Run 或系统占用 → 该节点 failed |
| `preferred` | 先试 `port`，占了就往上找（+1…+N，N≤50） |
| `auto` | bind `0` 拿系统分配，或从 18000 起扫 |

注入：spawn/write 前把 command 里的 `{PORT}` 换成 `allocated`。**只改这次要 write 的字符串，不改 graph.command。**

停流程：`canvas_run_stop` 清 `leases`（端口随进程死而释放；不要单独做全局端口守护除非已有）。

**本步验收:** 单测分配器：fixed 冲突失败；preferred 被占则换；auto 两次 Run 可以不同端口。断言 `freeze_plan` 后的 graph clone 没有 lease。

---

## 步骤 3 — 就绪：输出正则（先做）

service 节点不要等 exit。改为：

1. start：spawn/绑定 shell，write 替换后的 command（通常前台服务，不会自己 exit）
2. 订阅该 agent 的 PTY 输出（现有 channel / output_hub）。匹配 `readyPattern`（Rust `regex` crate 已在依赖里）→ 该节点 `ready`（status 枚举加 `ready`，与 task 的 `ok` 并列）
3. 超时（默认 30s，可以后做成图字段）：`failed`，下游 blocked
4. service **自己** exit 非 0：若尚未 ready → failed；已 ready 是否拆下游本期 **不拆**（避免 dev server 崩溃连带杀的产品争论）。记在代码注释：v1 已 ready 的 service 退出只标 `failed`，不自动 stop 整次 Run。

单测：用假输出字节喂匹配 / 不匹配超时。

**本步验收:** 假输出 `"Listening on 127.0.0.1:3000"` + pattern `Listening on` → ready，下游可 start。

---

## 步骤 4 — 就绪：TCP listen（紧随）

对已分配 `listen` 做周期性 `TcpStream::connect`（100–200ms，总超时同步骤 3）。

输出正则 **或** TCP 成功即 ready（谁先谁算）。两者都没配：service 在 start 后立刻 ready（并在校验里 warn——实现上 `validate` 给 `kind=service` 且无 pattern 无 portPolicy 一条软警告不够硬；**改为必须至少有 `readyPattern` 或 `portPolicy`**）。

**本步验收:** 单测用 `std::net::TcpListener::bind(127.0.0.1:0)` 假服务，Run 能等到。

---

## 步骤 5 — 前端：kind 切换 + 状态

- 终端卡：一个 `task | service` 切换（单对象动作，blur/`change` → set 该节点 kind）
- 可选：`readyPattern`、`portPolicy`、`port` 三个小输入。没有也行，先手改 JSON + 步骤 7 手动
- 卡片状态：`ready` 用 `--success`；`blocked` 用 `--warn`；显示 Run 里的 `listen` 地址（只读，来自 `canvas_run_status`，**不是** graph）

i18n：`kindTask` / `kindService` / `ready` / `listenAt`。

**本步验收:** `tsc`；画布能看出谁 ready、下游谁在等。

---

## 步骤 6 — 手动端到端

1. service 卡 command：`python -m http.server {PORT}`（或 `python3`），`portPolicy: auto`，`readyPattern` 可空、靠 TCP。
2. 下游 task：`curl -sf http://127.0.0.1:{PORT}/ >/dev/null`（注入的是 Run 的 PORT，不是图里的）。
3. 从 service 运行：service ready 之后 curl 才跑，exit 0。
4. 停 Run：python 进程没了。
5. 再跑一次 auto：端口可以变；`graph.json` 两次 diff **没有**端口数字变化（command 仍含 `{PORT}` 字面量）。
6. 把 service 改成 bind 已占用 fixed 端口：节点 failed，curl blocked。

---

## 步骤 7 — 文档

- 契约 §4 实现已对齐：补 RUNBOOK「端口租约只活在 Run」
- `canvas-view.md` 状态 → `期 3 已落地`
- 本文件顶部：`状态: 已落地`

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
```

---

## 完成清单（期 3 出门）

- [ ] service 未就绪时下游不盲启
- [ ] `{PORT}` 注入只发生在 Run write
- [ ] `canvas_graph_get` 无 listen / allocatedPort
- [ ] 停流程反向清理 + 租约丢掉
- [ ] auto 两次 Run 端口可变、图不变
- [ ] 单测覆盖分配冲突、正则 ready、TCP ready、图白名单
