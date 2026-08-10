# Phase 1：Master–Worker MVP 实施计划

> **目标：** 在不引入 LangGraph、A2A、ACP 等新框架，也不大规模重构现有架构的前提下，基于当前 CaPilot IDE 的 Rust/Tauri、Dispatcher、PTY、SQLite 和 React/Zustand，实现第一个可完全通过 UI 验收的单 Master + 单 Worker Task 闭环。
>
> **范围：** 本文只规划 Phase 1，不包含多 Worker DAG、自动重试、名字包上传、自动 Debug Loop、自动合并或远程 Worker。

---

## 1. 目标闭环

```text
用户在 UI 输入
  ↓
Composer 把协作规则和用户需求发送给 Master
  ↓
Master 执行 capilot dispatch
  ↓
Dispatcher 创建并持久化 Task
  ↓
Dispatcher 向 Worker PTY 下发任务
  ↓
Worker 执行并携带 task_id report
  ↓
Dispatcher 更新 Task
  ├─ 发 Task UI event
  └─ 把结果发送给 Master
  ↓
UI Task Activity 展示完整状态
```

本阶段继续复用：

- Rust/Tauri 主进程。
- `Dispatcher`。
- `PtyManager`。
- Unix Socket。
- `capilot` shim。
- SQLite。
- Zustand。
- React 右侧栏。
- 长期运行的交互式 Worker PTY。

本阶段暂时保留“Worker 按任务提示执行 `capilot report`”作为完成契约。自动识别各 CLI 单轮结束延期处理。

---

## 2. Task 数据模型

### 2.1 Rust 状态枚举

新增文件：

```text
src-tauri/src/orchestration/task.rs
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
```

状态转换限定为：

```text
queued
  ├─→ running
  │    ├─→ succeeded
  │    ├─→ failed
  │    └─→ cancelled
  ├─→ failed
  └─→ cancelled
```

第一版不允许终态重新进入运行状态：

```text
succeeded → running
failed    → running
cancelled → running
```

重新执行时创建新的 `task_id`，不复用旧任务。

### 2.2 Rust Task struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub project_id: String,
    pub master_agent_id: String,
    pub worker_agent_id: String,
    pub worker_display_name: String,

    pub title: String,
    pub prompt: String,
    pub status: TaskStatus,

    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,

    pub result: Option<String>,
    pub error: Option<String>,
    pub artifact: Option<serde_json::Value>,
}
```

前后端事件直接复用这个结构，避免再维护另一套 Task DTO。

### 2.3 SQLite 表

继续使用现有数据库：

```text
~/CaPilot/sessions.db
```

在 `persistence.rs` 的数据库初始化中增加：

```sql
CREATE TABLE IF NOT EXISTS tasks (
    task_id             TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL,
    master_agent_id     TEXT NOT NULL,
    worker_agent_id     TEXT NOT NULL,
    worker_display_name TEXT NOT NULL,

    title               TEXT NOT NULL,
    prompt              TEXT NOT NULL,
    status              TEXT NOT NULL,

    created_at          INTEGER NOT NULL,
    started_at          INTEGER,
    finished_at         INTEGER,

    result              TEXT,
    error               TEXT,
    artifact            TEXT
);

CREATE INDEX IF NOT EXISTS idx_tasks_project_created
ON tasks(project_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_tasks_worker_status
ON tasks(worker_agent_id, status);
```

`artifact` 保存 JSON 字符串。第一版允许一直为 `NULL`。

第一版不拆出 `task_results` 表。单任务只有一次结果，直接放在 `tasks` 中。未来引入 Attempt、多次重试或大型 artifact 时再拆分。

### 2.4 字段用途

| 字段 | 第一版 | 原因 |
|---|---|---|
| `task_id` | 必须 | dispatch、report、UI、存储之间唯一关联 |
| `project_id` | 必须 | 避免跨项目调度和查询 |
| `master_agent_id` | 必须 | 结果应该返回给哪个 Master |
| `worker_agent_id` | 必须 | 调度使用稳定内部 ID |
| `worker_display_name` | 必须 | 保存执行当时的 UI 名称快照 |
| `title` | 必须 | Task Activity 的短标题 |
| `prompt` | 必须 | Worker 实际收到的任务 |
| `status` | 必须 | UI 生命周期 |
| `created_at` | 必须 | 显示任务创建时间 |
| `started_at` | 必须 | 区分已创建和已执行 |
| `finished_at` | 必须 | 显示耗时和终态 |
| `result` | 必须 | Worker 成功结果 |
| `error` | 必须 | Worker 失败原因 |
| `artifact` | 只预留 | Phase 1 不解析 Commit、Diff、TestResult |

`worker_display_name` 虽然可以从 sessions 表查询，但仍需要保存：

- Worker 后续可能改名。
- 历史任务应该显示执行时的名称。
- UI 恢复时不依赖 Agent session 仍然存在。

### 2.5 Task title

Phase 1 不为标题增加新的 LLM 调用。

建议 Master 调用：

```bash
capilot dispatch \
  --worker "阿比西尼亚" \
  --title "检查登录模块" \
  --prompt "检查登录模块的认证逻辑"
```

为了兼容现有调用，也允许旧形式：

```bash
capilot dispatch 阿比西尼亚 "检查登录模块"
```

旧形式下，`title` 取 prompt 第一行，最多 80 个字符。

---

## 3. 后端修改计划

### 3.1 新增 `src-tauri/src/orchestration/task.rs`

包含：

- `TaskStatus`。
- `TaskRecord`。
- `TaskDispatchRequest`。
- `TaskReportRequest`。
- 合法状态转换检查。
- UI event 名称常量。

建议请求结构：

```rust
pub struct TaskDispatchRequest {
    pub worker_reference: String,
    pub title: Option<String>,
    pub prompt: String,
}

pub struct TaskReportRequest {
    pub task_id: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub artifact: Option<serde_json::Value>,
}
```

第一版 `TaskReportRequest.status` 只接受 `succeeded` 和 `failed`。`cancelled` 由 CaPilot UI/Dispatcher 产生。

### 3.2 修改 `src-tauri/src/orchestration/mod.rs`

增加：

```rust
pub mod task;
pub use task::{TaskRecord, TaskStatus};
```

### 3.3 修改 `src-tauri/src/persistence.rs`

增加 `tasks` 表和索引，并在 `SessionsDb` 中新增：

```rust
pub fn task_insert(&self, task: &TaskRecord) -> Result<()>;

pub fn task_get(
    &self,
    task_id: &str,
) -> Result<Option<TaskRecord>>;

pub fn task_list_by_project(
    &self,
    project_id: &str,
    limit: usize,
) -> Result<Vec<TaskRecord>>;

pub fn task_mark_running(
    &self,
    task_id: &str,
    started_at: i64,
) -> Result<bool>;

pub fn task_complete(
    &self,
    task_id: &str,
    result: &str,
    artifact: Option<&serde_json::Value>,
    finished_at: i64,
) -> Result<bool>;

pub fn task_fail(
    &self,
    task_id: &str,
    error: &str,
    finished_at: i64,
) -> Result<bool>;

pub fn task_cancel(
    &self,
    task_id: &str,
    finished_at: i64,
) -> Result<bool>;

pub fn task_list_unfinished(&self) -> Result<Vec<TaskRecord>>;
```

更新语句必须限制当前状态，例如：

```sql
UPDATE tasks
SET status = 'succeeded',
    result = ?2,
    finished_at = ?3
WHERE task_id = ?1
  AND status = 'running';
```

使用受影响行数判断更新是否成功，从而阻止：

- 同一 report 重复完成。
- 已取消任务又被改为成功。
- 同一 Task 被多次写入终态。

### 3.4 修改 `src-tauri/src/orchestration/dispatcher.rs`

#### WorkerState 调整

当前：

```rust
struct WorkerState {
    status: WorkerStatus,
    last_task: Option<String>,
}
```

修改为：

```rust
struct WorkerState {
    status: WorkerStatus,
    active_task_id: Option<String>,
    last_task_title: Option<String>,
}
```

Phase 1 限定一个 Worker 同时最多执行一个 Task。

#### 新增 Task 创建入口

```rust
fn create_and_dispatch_task(
    &self,
    request: TaskDispatchRequest,
    app: &AppHandle,
) -> Result<TaskRecord, String>;
```

内部顺序：

1. 读取当前 `master_id`。
2. 查询 Master session，得到 `project_id`。
3. 在当前 project 内解析 Worker。
4. 检查 Worker 存活且不 Busy。
5. 生成 `task_id`。
6. 创建 `queued` Task。
7. 写入 SQLite。
8. 发出 queued UI event。
9. 将 Worker 标为 Busy 并绑定 `active_task_id`。
10. 更新 Task 为 running。
11. 发出 running UI event。
12. 给 Worker PTY 下发带完成契约的 prompt。

#### Task ID

使用项目已有的 `uuid` crate：

```rust
let task_id = format!("task_{}", Uuid::new_v4().simple());
```

例如：

```text
task_4fd8b43ea3d54c719f4a14a28f63a4e1
```

UI 只显示短格式 `task_4fd8b43e`，存储和通信始终使用完整 ID。

#### Worker prompt 包装

Dispatcher 下发：

```text
[CaPilot Task]
Task ID: task_4fd8b43e...
Title: 检查登录模块

任务：
检查登录模块的认证逻辑。

完成规则：
- 成功时执行：
  capilot report task_4fd8b43e... succeeded "<结果摘要>"
- 失败时执行：
  capilot report task_4fd8b43e... failed "<错误原因>"
- 不要使用其他 task_id。
```

用户原始 prompt 仍单独保存在数据库。

#### 下发失败处理

如果 Task 已创建但 PTY 写入失败：

```text
queued → failed
```

保存错误和完成时间，同时：

- Worker 恢复 Idle。
- 清空 `active_task_id`。
- 发出 failed UI event。
- dispatch 返回错误及 `task_id`。

#### Task report

将当前按 Worker 名称处理的 report 调整为：

```rust
fn report_task(
    &self,
    request: TaskReportRequest,
    app: &AppHandle,
) -> String;
```

处理顺序：

1. 根据 `task_id` 查询 SQLite。
2. Task 不存在则报错。
3. Task 已是终态则拒绝重复 report。
4. 验证 Task 当前为 running。
5. 根据 status 更新 succeeded 或 failed。
6. 清空对应 Worker 的 `active_task_id`。
7. Worker 返回 Idle。
8. 发出完整 Task UI event。
9. 将带 `task_id` 的结果发送给 Task 记录中的 Master。

开发期间可以保留旧命令：

```bash
capilot report 阿比西尼亚 "摘要"
```

但正式 Task 路径必须使用：

```bash
capilot report <task_id> succeeded "<结果>"
capilot report <task_id> failed "<错误>"
```

新 Task 不再通过 Worker 名称关联 report。

### 3.5 Worker 名称解析

将：

```rust
resolve_worker(reference)
```

修改为：

```rust
resolve_worker_in_project(
    project_id: &str,
    reference: &str,
) -> Result<AgentSessionRecord, WorkerResolveError>;
```

匹配顺序：

1. 完整内部 ID。
2. 完整显示名称。
3. Phase 1 不进行显示名称模糊匹配。
4. ID 前缀只有唯一匹配时才成功。

错误类型：

```rust
enum WorkerResolveError {
    NotFound {
        reference: String,
        available_names: Vec<String>,
    },
    Ambiguous {
        reference: String,
        candidates: Vec<String>,
    },
}
```

名字不存在时返回：

```text
找不到 Worker“加菲猫”。
当前项目可用 Worker：阿比西尼亚、布偶、暹罗。
```

### 3.6 修改 `src-tauri/src/orchestration/shim.rs`

支持：

```bash
capilot dispatch \
  --worker "阿比西尼亚" \
  --title "检查登录模块" \
  --prompt "检查登录模块的认证逻辑"

capilot report \
  task_4fd8b43e... \
  succeeded \
  "登录模块检查完成"

capilot report \
  task_4fd8b43e... \
  failed \
  "测试命令执行失败"
```

dispatch 成功输出：

```json
{
  "ok": true,
  "task_id": "task_4fd8b43e...",
  "status": "running",
  "worker_id": "agent_a8217c...",
  "worker_display_name": "阿比西尼亚"
}
```

report 成功输出：

```json
{
  "ok": true,
  "task_id": "task_4fd8b43e...",
  "status": "succeeded"
}
```

Phase 1 建议 shim 和 socket 改成 JSONL：一行请求 JSON、一行响应 JSON。不需要引入完整 JSON-RPC。

### 3.7 修改 `src-tauri/src/lib.rs`

新增 Tauri commands：

```rust
#[tauri::command]
async fn task_list(
    persistence: State<'_, Arc<Persistence>>,
    project_id: Option<String>,
) -> Result<Vec<TaskRecord>, String>;

#[tauri::command]
async fn task_get(
    persistence: State<'_, Arc<Persistence>>,
    task_id: String,
) -> Result<Option<TaskRecord>, String>;

#[tauri::command]
async fn task_cancel(
    dispatcher: State<'_, Arc<Dispatcher>>,
    app: AppHandle,
    task_id: String,
) -> Result<TaskRecord, String>;
```

注册到 `generate_handler!`。

取消处理：

1. 查询 Task。
2. 仅允许 queued/running 取消。
3. 若 running，向 Worker PTY 写入 `Ctrl+C`。
4. 更新 Task 为 cancelled。
5. Worker 恢复 Idle。
6. 发出 Task UI event。
7. 通知 Master 任务已取消。

Phase 1 不需要前端直接 dispatch，正常入口仍是用户通过 Master 调度。

### 3.8 Master 输入契约

修改：

```text
ui/components/layout/Composer.tsx
```

对发送给 Master 的用户消息增加精简协作说明，逻辑上等价于：

```text
你是 CaPilot Master。

当用户点名 Worker 时：
1. 运行 capilot status 获取当前项目 Worker。
2. 使用完整显示名称调用 capilot dispatch。
3. dispatch 返回 task_id 后等待 Worker report。
4. 收到相关 Task 结果后再回复用户。
5. 名字不存在时告诉用户可用名称，不要自行选择相似名称。

用户需求：
让阿比西尼亚检查登录模块。
```

Phase 1 明确每次只派发一个 Worker Task。

此规则随 Master 消息发送，不修改 Claude/Codex 全局配置，也不安装 Agent hook。

---

## 4. 修改后的消息流

### 4.1 正常成功路径

```text
1. 用户在 UI 输入：
   “让阿比西尼亚检查登录模块”

2. Composer 把 Master 协作规则和用户需求发送给 Master PTY

3. Master 执行：
   capilot status

4. Master 得到当前项目 Worker 列表

5. Master 执行：
   capilot dispatch
     --worker "阿比西尼亚"
     --title "检查登录模块"
     --prompt "检查登录模块并返回发现的问题"

6. Dispatcher 在当前 Master project 内解析：
   阿比西尼亚 → agent_a8217c

7. Dispatcher 生成：
   task_4fd8b43e...

8. SQLite 插入 queued Task

9. UI 收到 orchestration://task queued event

10. Dispatcher 绑定 Worker active_task_id

11. SQLite 更新为 running，写入 started_at

12. UI 收到 running event

13. Dispatcher 将带 task_id 的任务写入 Worker PTY

14. Worker 完成并执行：
    capilot report task_4fd8b43e... succeeded "检查完成……"

15. Dispatcher 按 task_id 查询 Task

16. SQLite 更新为 succeeded，保存 result 和 finished_at

17. Worker 返回 Idle

18. UI 收到 succeeded event

19. Dispatcher 给 master_agent_id 对应的 Master PTY 发送结果

20. Master 向用户生成最终回复
```

### 4.2 Report 关联规则

唯一关联依据是 `task_id`，不再用 Worker 名称关联 Task。

收到 report 时校验：

- Task 是否存在。
- Task 是否为 running。
- Task 是否已经处于终态。
- Task 绑定的 Worker 是否与当前 active task 一致。
- `WorkerState.active_task_id` 是否等于该 `task_id`。

Phase 1 一个 Worker 同时只能有一个 active task。

未来多 Worker 并行时，每个 Worker 使用不同 `task_id`，结果不依赖到达顺序，也不依赖显示名称。

---

## 5. UI Task Activity

### 5.1 面板位置

在右侧栏 Overview 中增加 `Task Activity`：

```text
OverviewDashboard
Task Activity
Master Agent Report
```

第一版不制作 DAG，也不新增复杂页面。

### 5.2 运行中卡片

```text
┌──────────────────────────────┐
│ 🟡 检查登录模块              │
│                              │
│ Worker  阿比西尼亚 · Codex   │
│ 状态    执行中               │
│ 开始    14:32:08             │
│ 已用时  18s                  │
│                              │
│ 正在检查登录模块的认证逻辑…   │
│                       [停止] │
└──────────────────────────────┘
```

### 5.3 成功卡片

```text
┌──────────────────────────────┐
│ ✅ 检查登录模块              │
│                              │
│ Worker  阿比西尼亚           │
│ 状态    已成功               │
│ 耗时    42s                  │
│                              │
│ 结果                         │
│ 发现 token 过期判断存在问题… │
│                 [查看终端]   │
└──────────────────────────────┘
```

### 5.4 失败卡片

```text
┌──────────────────────────────┐
│ ❌ 检查登录模块              │
│                              │
│ Worker  阿比西尼亚           │
│ 状态    失败                 │
│ 耗时    15s                  │
│                              │
│ 错误                         │
│ 无法运行测试命令             │
│                 [查看终端]   │
└──────────────────────────────┘
```

### 5.5 取消卡片

```text
┌──────────────────────────────┐
│ ⚪ 检查登录模块              │
│ Worker  阿比西尼亚           │
│ 状态    已取消               │
│                 [查看终端]   │
└──────────────────────────────┘
```

### 5.6 第一版显示字段

列表折叠状态：

- Task title。
- 状态图标。
- Worker display name。
- 创建时间或耗时。

展开状态：

- 完整 prompt。
- result 或 error。
- task_id 短格式。
- created/started/finished 时间。
- 查看 Worker 终端。
- running 时显示停止按钮。

`artifact` 第一版不渲染。为空时不显示“修改文件”区域。

### 5.7 React 文件修改

#### `ui/state/store.ts`

增加：

```ts
export type TaskStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface TaskRecord {
  task_id: string;
  project_id: string;
  master_agent_id: string;
  worker_agent_id: string;
  worker_display_name: string;
  title: string;
  prompt: string;
  status: TaskStatus;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  result: string | null;
  error: string | null;
  artifact: unknown | null;
}
```

Store 增加：

```ts
tasks: TaskRecord[];
setTasks(tasks: TaskRecord[]): void;
upsertTask(task: TaskRecord): void;
```

按 `created_at DESC` 排序。

#### `ui/state/orchestration.ts`

初始化时增加：

```ts
invoke<TaskRecord[]>("task_list")
```

订阅：

```text
orchestration://task
```

收到事件后调用 `upsertTask(task)`。

#### UI 组件

新增：

```text
ui/components/orchestration/TaskActivity.tsx
ui/components/orchestration/TaskCard.tsx
```

修改：

```text
ui/components/layout/RightSidebar.tsx
```

挂载 `<TaskActivity />`。

#### `ui/components/layout/Composer.tsx`

- 保留用户原文。
- 添加 Master 使用 `capilot status/dispatch` 的协作契约。
- 加入单 Worker Phase 1 限制。
- 名字不存在时要求返回可用名称。

#### `ui/App.css`

增加：

- Task Activity 容器。
- queued/running/succeeded/failed/cancelled 状态色。
- Task 卡片展开/收起。
- 结果和错误区域。
- 停止按钮。
- 时间和 Worker meta。

### 5.8 UI 重启恢复

应用启动时：

```text
useOrchestrationSync
→ invoke task_list
→ SQLite 返回最近 Task
→ Zustand 恢复
→ Task Activity 显示历史
```

对于重启前仍为 running、且 Worker PTY 已不存在的任务：

```text
running → failed
error = "CaPilot 重启或 Worker 会话中断，任务未确认完成"
```

不要恢复为 running，也不要自动重新派发。

---

## 6. UI 验收案例

### 案例 1：单 Worker 成功

前置：

- UI 中存在 Worker“阿比西尼亚”。
- Worker 已启动并处于空闲状态。

输入：

> 让阿比西尼亚检查 README 是否有明显错误，完成后告诉我。

应该看到：

1. Task Activity 出现“检查 README”任务。
2. Worker 显示“阿比西尼亚”。
3. 状态从“排队中”变成“执行中”。
4. 阿比西尼亚终端开始执行任务。
5. 完成后卡片变为绿色“已成功”。
6. 卡片中出现 Worker 结果。
7. Master 终端收到 Task 结果。
8. Master 在 UI 中给出最终回复。

验收重点：不需要手动进入 Worker 输入命令。

### 案例 2：Worker 名字不存在

输入：

> 让加菲猫检查 README。

应该看到：

1. 不向任意 Worker 派发任务。
2. 不出现“加菲猫执行中”。
3. Master 告知找不到“加菲猫”。
4. Master 显示当前项目可用 Worker 名称。
5. 已有 Worker 状态保持不变。

名称解析应在创建 Task 前完成，因此本案例通常不创建 Task。

### 案例 3：Worker 执行失败

输入：

> 让阿比西尼亚执行一个不存在的测试命令，并如实报告错误。

应该看到：

1. Task 卡片出现。
2. 状态变为“执行中”。
3. Worker 报告失败后，卡片变为红色“失败”。
4. 错误区域显示简短原因。
5. Worker 回到空闲状态。
6. Master 明确告知任务失败。

### 案例 4：取消任务

输入：

> 让阿比西尼亚执行一个需要较长时间的检查。

任务进入执行中后点击“停止”。

应该看到：

1. 状态变成“已取消”。
2. 卡片记录完成时间。
3. 阿比西尼亚停止当前工作并返回空闲。
4. 后续 success report 不能把 Task 改成成功。
5. Master 收到任务取消信息。

### 案例 5：重启恢复

操作：

1. 完成一个成功任务。
2. 再启动一个执行中的任务。
3. 关闭 CaPilot IDE。
4. 重新打开。

应该看到：

1. 之前成功的任务仍存在。
2. 成功结果仍可展开查看。
3. 重启前未确认完成的任务显示失败。
4. 错误说明任务因应用或 Worker 中断而未确认完成。
5. 不会永久显示“执行中”。
6. 不会自动重新派发旧任务。

---

## 7. 实施顺序

### Step 1：Task 表和 Rust 数据模型

修改：

- 新增 `orchestration/task.rs`。
- 修改 `orchestration/mod.rs`。
- 修改 `persistence.rs`。
- 增加 Task CRUD 和状态转换测试。

此步不改变 Dispatcher、socket、UI 和 Agent 行为。

UI 回归验收：

1. IDE 正常启动。
2. 原有 session 正常显示。
3. Master/Worker 正常打开。
4. 新终端正常创建。
5. 重启后 session 正常恢复。

这是唯一一个没有新增可见功能的步骤。不能为了让数据库表可见而提前制作临时 UI。

### Step 2：Dispatcher 创建并运行 Task

修改：

- `dispatcher.rs` 生成 task_id。
- 在当前项目解析 Worker。
- 插入 queued、更新 running。
- WorkerState 绑定 `active_task_id`。
- 修改 shim 命令解析。
- 修改 Composer 的 Master 协作提示。

UI 验收：

1. 对 Master 说“让阿比西尼亚检查 README”。
2. 阿比西尼亚自动收到任务。
3. Worker 状态变为 Busy。
4. Worker prompt 中可以看到 Task ID。
5. 名字不存在时 Master 给出可用名称。

此时尚没有 Task Activity 卡片。

### Step 3：Worker report 回写 Task

修改：

- `capilot report` 要求 task_id。
- Dispatcher 根据 task_id 更新 Task。
- 增加成功、失败和重复 report 状态约束。
- Worker 返回 Idle。
- 结果发送给正确 Master。
- Worker 异常退出时将 active Task 标为失败。

UI 验收：

1. Worker 完成后从 Busy 回到 Idle。
2. Master Report 显示结果。
3. Master 收到结果并回复。
4. Worker 失败时 Master Report 显示失败。
5. 同一 Task 重复 report 不产生两次完成。

### Step 4：Task Activity UI

修改：

- `store.ts` 增加 Task 状态。
- `orchestration.ts` 拉取 Task 并监听 Task event。
- 新增 `TaskActivity.tsx` 和 `TaskCard.tsx`。
- `RightSidebar.tsx` 挂载组件。
- `App.css` 增加样式。

UI 验收：

```text
任务创建
→ 排队中
→ 执行中
→ 已成功/失败
→ 结果显示
```

案例 1、2、3 应全部通过。

### Step 5：取消与重启恢复

修改：

- `lib.rs` 增加 `task_cancel`。
- Dispatcher 增加取消逻辑。
- Task Card 增加停止按钮。
- 启动时处理遗留 running Task。
- Task Activity 从 SQLite 恢复。

UI 验收：

- 案例 4：取消任务。
- 案例 5：关闭并重启恢复。

### Step 6：Phase 1 稳定化

后端测试至少覆盖：

- Task insert/get/list。
- queued → running。
- running → succeeded。
- running → failed。
- running → cancelled。
- 终态不能再次改变。
- 不存在的 task_id report。
- 错误 Worker 名称。
- 同名 Worker 歧义。
- 跨项目 Worker 不可解析。
- Worker 异常退出导致 Task failed。
- 启动时遗留 running Task 变 failed。

UI 回归覆盖：

- Master 和普通终端创建。
- Agent resume。
- Worker 角色开关。
- Worker terminal 输入锁。
- Master Report。
- Task Activity。
- 任务取消。
- 重启恢复。

---

## 8. Phase 1 完成定义

以下条件全部满足，Phase 1 才算完成：

- 用户只在 Composer 对 Master 输入自然语言。
- Master 能按 UI 中的完整 Worker 名称派发一个任务。
- Dispatcher 创建唯一 `task_id`。
- Task 被保存进 SQLite。
- UI 显示 queued、running 和最终状态。
- Worker 成功和失败都能关联正确 Task。
- Task 结果自动发送给正确 Master。
- Master 能在收到结果后回复用户。
- 名字不存在时不会误派。
- Worker 或应用中断时 Task 不会永久 running。
- 应用重启后 Task 历史仍可见。
- 用户无需查看终端命令、JSON、SQLite 或日志来验收。

明确不包含：

- 多 Worker DAG。
- 自动重试。
- ACP。
- A2A。
- 名字包上传。
- 自动 Debug Loop。
- 自动合并代码。
- 远程 Worker。
- 多 Attempt。

这套范围在不替换当前 Rust/PTY/Dispatcher 架构的前提下，建立第一个真正可由 UI 验收的 Master–Worker Task 闭环。
