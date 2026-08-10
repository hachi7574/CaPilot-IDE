# Step 3：基于 task_id 的 Worker Report 完成闭环

## 完成结论

Step 3 已完成。正式 Task 闭环现在是：

```text
Worker
  → Rust capilot report
  → reporter_agent_id + task_id
  → Dispatcher 身份与状态校验
  → SQLite succeeded / failed
  → 仅释放仍绑定该 task_id 的 Worker
  → 现有 orchestration report/event
  → 正确 Master 的结构化 Task Result
```

本阶段没有新增 Task Activity UI、React Task store、多 Worker、DAG、重试、Debug Loop 或发布/sidecar 逻辑。

## 修改文件

### `src-tauri/src/bin/capilot.rs`

- 正式支持：
  - `capilot report <完整 task_id> succeeded "<result>"`
  - `capilot report <完整 task_id> failed "<error>"`
- report status 只接受 `succeeded` / `failed`。
- 自动读取 `CAPILOT_AGENT_ID`，不提供手动 `--agent-id` 参数。
- 没有 Agent 身份时在 CLI 边界返回结构化 `invalid_request`，不会发送匿名 report。
- 成功 report 发送 `result`；失败 report 发送 `error`，二者不会同时出现。
- 中文、空格、引号和单参数内的多行内容继续由 argv + serde_json 安全传输。
- CLI 仍只做参数、身份字段、JSON 和 Unix socket，不包含 Task 状态业务。

### `src-tauri/src/lib.rs`

- 每次创建/恢复 Agent PTY 时注入：

  ```text
  CAPILOT_AGENT_ID=<内部 agent ID>
  ```

- 该变量在 runtime adapter 自有环境之后追加，避免 adapter 意外覆盖。
- CaPilot 启动时调用 `task_fail_unfinished`，将上次进程遗留的 queued/running Task 统一标记 failed。
- 重启错误固定为：

  ```text
  CaPilot restarted before task completion was confirmed
  ```

### `src-tauri/src/orchestration/task.rs`

- `TaskReportRequest` 增加 `reporter_agent_id`。
- 继续复用已有 `TaskStatus`、result/error/artifact 预留字段，没有新增平行状态模型。

### `src-tauri/src/orchestration/dispatcher.rs`

- JSON report wire 改为正式 task-aware 字段。
- 使用 `task_get(task_id)` 唯一定位 Task，不根据 Worker 名称猜测。
- 校验 Task 存在、status 为 running、reporter 内部 ID 与 `worker_agent_id` 完全一致。
- 成功调用 Step 1 的 `task_complete`，失败调用 `task_fail`。
- 仅当 `WorkerState.active_task_id == task_id` 时清除 Task 绑定并恢复 Idle。
- 扩展现有 `WorkerReport` 事件，附加可选 `task_id/status`；旧 UI 所需的 worker/summary/level/ts 仍保持。
- 根据 Task 持久化的 `master_agent_id` 投递结果，不依赖当前全局 Master 猜测。
- Master 收到结构化 Task Result；不注入 Worker terminal history。
- Master 注入内容限制为 8,000 个 Unicode 字符；完整主动 report 仍保存在 SQLite，超长时附加截断说明。
- legacy name-based raw socket report 被明确标为 deprecated：可以展示旧消息，但不能修改 Task 或释放 Worker。
- natural exit 与 stale PTY sweeper 都会失败 active Task，防止 running 泄漏。

### `src-tauri/src/persistence.rs`

- 新增 `task_fail_unfinished(error, finished_at)`。
- 单条受限 SQL 只更新 queued/running，已有 succeeded/failed/cancelled 终态不受影响。

### 文档

- 更新 `docs/implementation/rust-capilot-cli-report.md` 中已经过时的 Step 2 report 描述。

## Rust CLI report 格式

成功 wire：

```json
{
  "type": "report",
  "task_id": "task_xxx",
  "reporter_agent_id": "agent_xxx",
  "status": "succeeded",
  "result": "README 检查完成",
  "error": null
}
```

失败 wire：

```json
{
  "type": "report",
  "task_id": "task_xxx",
  "reporter_agent_id": "agent_xxx",
  "status": "failed",
  "result": null,
  "error": "测试命令失败"
}
```

Dispatcher 成功响应包含：

- `ok: true`
- 完整 `task_id`
- `status: succeeded|failed`
- 内部 `worker_id`
- `worker_display_name`

## Worker 身份来源

身份链路为：

```text
Rust/Tauri build_and_spawn(id)
  → PTY environment CAPILOT_AGENT_ID=id
  → Worker 内启动 Rust capilot
  → CLI 自动读取环境
  → reporter_agent_id 写入 JSON
  → Dispatcher 对比 Task.worker_agent_id
```

显示名称只用于 Worker 列表、UI 与 Master Result，不参与身份判断。测试明确验证把“阿比西尼亚”作为 reporter ID 会被拒绝。

这是当前本机、同用户、受 CaPilot 管理 Agent 的轻量身份边界，不是面向不可信远程进程的密码学认证。它符合本阶段要求；未来远程 Worker 需要单独的凭据/transport 身份设计，不影响当前 Task 关联键。

## Dispatcher report 流程

1. 解析结构化 JSON。
2. status 字符串解析为现有 `TaskStatus`；非 succeeded/failed 最终拒绝。
3. reporter_agent_id 为空则拒绝。
4. `task_get(task_id)`；不存在返回 `task_not_found`。
5. Task 非 running 返回 `task_not_running`。
6. reporter 与 `worker_agent_id` 不一致返回 `reporter_mismatch`。
7. succeeded 必须且只能带非空 result；failed 必须且只能带非空 error。
8. 使用 SQL 前置状态约束执行 `task_complete` / `task_fail`。
9. SQL 返回 0 行时返回 `task_transition_rejected`，处理并发重复 report。
10. 尝试按 active_task_id 安全释放 Worker。
11. 记录并发出现有 orchestration report。
12. 向 Task 自己的 master_agent_id 投递结构化结果。
13. 返回结构化成功 JSON 给 Worker CLI。

## Task 状态与重复保护

正式 report 只允许：

```text
running → succeeded
running → failed
```

以下情况全部拒绝，且 SQL 终态不会被覆盖：

- succeeded 后重复 succeeded。
- succeeded 后 failed。
- failed 后重复 failed。
- cancelled 后迟到 succeeded。
- queued/cancelled/succeeded/failed 上的正式 report。
- 不存在 task_id。
- reporter 身份错误或缺失。
- status 不是 succeeded/failed。
- succeeded 同时携带 error，或 failed 同时携带 result。

状态读取与 SQL 更新之间即使发生并发竞争，`task_complete/task_fail` 的 `WHERE status = 'running'` 仍保证只有一次成功。

## Worker 释放规则

Task 数据库更新与 Worker 内存释放是两个独立安全步骤：

```text
if WorkerState.active_task_id == reported task_id:
    status = Idle
    active_task_id = None
    current_task_title = None
else:
    不修改 WorkerState
```

因此旧 Task 的迟到 report 可以在合法时完成旧数据库记录，但绝不能把正在执行新 Task 的 Worker 错误释放为 Idle。

被正确释放时继续发出已有 `orchestration://event`，现有 UI 会显示 Worker 从 Busy 恢复 Idle。

## Worker 异常退出

现有 PTY natural-exit callback 是已确认的真实进程结束边界。若 Worker 当时 Busy 且有 active_task_id：

1. 生成 failed Task report。
2. error 为：

   ```text
   Worker process exited before reporting task completion (exit=<code>)
   ```

3. 使用同一 `apply_task_report` 状态与身份路径更新 Task。
4. 清除 active Task、恢复 Idle、发出 Worker 事件。
5. 向正确 Master 发送 failed Task Result。
6. 保留现有 Worker attention/error 行为。

stale-busy sweeper 不再直接清空 Worker；它也进入上述失败路径，避免 sweeper 与 natural-exit callback 竞争时遗留 running Task。并发到达时 SQL 终态约束保证只完成一次。

Agent CLI 一次对话结束但进程仍在等待下一条输入时不会触发 natural-exit callback，因此不会被误判。

## CaPilot 重启恢复

Persistence 打开后、Dispatcher/Agent 恢复前，启动逻辑执行：

```sql
UPDATE tasks
SET status = 'failed', error = <restart error>, finished_at = <now>
WHERE status IN ('queued', 'running')
```

不重试、不重新 dispatch、不恢复 running。终态 Task 保持原样。

由于此时 Master PTY 和 React Task UI 尚未启动，重启收敛结果只持久化并记录日志，不做历史结果重新注入；后续 Task Activity UI 可以直接读取这些 failed 记录。

## Master Result 格式

成功：

```text
[CaPilot Task Result]

Task ID: task_xxx
Worker: 阿比西尼亚
Status: succeeded

Result:
README 检查完成……
```

失败：

```text
[CaPilot Task Result]

Task ID: task_xxx
Worker: 阿比西尼亚
Status: failed

Error:
测试命令失败……
```

投递目标来自 Task.master_agent_id。Master 不接收 Worker 完整终端历史，只接收 Worker 主动提交的 result/error。超过 8,000 字符时只截断 Master 上下文副本，SQLite 保留完整 report。

## 自动测试

新增/扩展覆盖：

- Rust CLI 成功 report 包含 task_id、reporter、status、result。
- CLI 拒绝非法 status。
- CLI 拒绝缺少 `CAPILOT_AGENT_ID`。
- 中文、多行、引号和较长内容保存与 JSON 传输。
- Agent spawn 身份环境变量名称和值。
- running → succeeded，result/finished_at 正确。
- running → failed，error/finished_at 正确。
- Worker Idle、active_task_id/title 清空。
- 正确 Master ID 与结构化成功/失败消息。
- 现有 report 队列收到 task_id/status。
- 错误 Worker、显示名称冒充、空身份均拒绝。
- task_id 不存在。
- 重复成功、成功后失败、重复失败。
- cancelled 后迟到成功。
- 非 succeeded/failed 状态。
- active_task_id 不匹配时不释放 Worker。
- natural process exit 失败 active Task。
- natural exit 使用统一失败路径；stale sweeper 的实现也复用该路径。
- 启动恢复只失败 unfinished Task，不修改终态。
- Master 上下文 8,000 Unicode 字符上限。
- Step 2 dispatch、project scope、Busy 拒绝、PTY 写失败回滚回归。
- Rust CLI status/dispatch/Unix socket 回归。

验证结果（2026-08-09）：

- `cargo test`：65 个 library tests + 5 个 Rust CLI tests，共 70 passed，0 failed。
- `cargo check --all-targets`：通过；仅有已有/后续预留 API 的 unused/dead-code 警告。
- `pnpm exec tsc --noEmit`：通过。
- `pnpm run build`：通过；仅有已有 Vite chunk size 提示。
- `git diff --check`：通过。
- 修改的 Rust 文件已执行 rustfmt。
- 仓库正式开发路径仍不包含 Python shim/runtime 调用。

## UI 验收步骤

### 准备

1. 完全退出旧 IDE 和 Agent 进程。
2. 执行 `pnpm tauri dev`，确保 Rust CLI 与新的 Agent 环境生效。
3. 在 Master project/分组创建或恢复 Master。
4. 在同一 project/分组创建“阿比西尼亚”，设为 Worker，并保持终端运行。
5. Composer 目标切换为 Master。

### 成功场景

输入：

> 让阿比西尼亚检查 README 是否有明显错误，完成后告诉我。

应看到：

1. Master 执行 `capilot status` 和 `capilot dispatch`。
2. dispatch 返回完整 task_id。
3. 阿比西尼亚变为 Busy。
4. Worker 自动收到 `[CaPilot Task]`、完整 Task ID 和 report 契约。
5. Worker 执行完成并自行运行 Rust `capilot report ... succeeded ...`。
6. CLI 返回 `ok: true`、同一 task_id 和 succeeded。
7. 阿比西尼亚恢复 Idle。
8. 现有 Master Report 区出现 Worker 结果。
9. Master 终端自动收到 `[CaPilot Task Result]`。
10. Master 基于结构化结果向用户给出最终回复，全程不需要进入 Worker 手工输入。

### 失败场景

让 Worker 执行一个会明确失败的检查。应看到：

```text
Busy → capilot report ... failed ... → Idle
```

Master Report 和 Master 终端都应显示 `Status: failed` 与 Error，而不是普通成功摘要。

### 身份检查（可选技术验收）

普通项目终端或系统 shell 没有 `CAPILOT_AGENT_ID` 时执行 report，会在 CLI 直接收到缺少身份的错误。不同 Worker 即使知道 task_id，其 reporter_agent_id 与 Task.worker_agent_id 不一致，也会收到 `reporter_mismatch`。

## 已知限制

- 仍是单 Master、单 Worker Task 模型；没有 group/request ID 或多 Worker 汇总。
- Agent 环境身份适用于当前可信本机 Worker，不是远程/恶意同用户进程的强认证。
- Master 不在线或 PTY 写入失败时，Task 仍会可靠进入终态并保留在 SQLite/当前 report 队列；本阶段没有持久化 Master inbox 重投。
- 重启遗留 Task 会可靠 failed，但没有 Task Activity UI，因此用户暂时不能在 UI 查看历史恢复记录。
- legacy name-based raw report 只展示旧消息，不完成 Task，也不释放 Worker。
- 超长 report 的完整内容保存在 Task；Master 注入截断到 8,000 字符。结构化 Artifact 留给后续阶段。
- report 成功依赖 Worker 遵守任务 prompt 主动调用 CLI；本阶段不解析终端输出猜测完成。

## Step 4 建议（未实施）

下一步应优先实现只读的 Task Activity UI：从后端读取 project scoped Task，订阅 Task 状态事件，并展示 queued/running/succeeded/failed/cancelled。不要同时引入多 Worker、DAG 或自动重试。这样用户可完全通过 UI 验证本阶段已持久化的生命周期和重启失败记录。
