# CaPilot IDE — 隔离工作区（git worktree）实现计划

> **日期:** 2026-08-15
> **状态:** 草案（待评审）
> **对标:** `docs/reference/worktree-isolation.md`（ORCA 的完整实现，本文只做本地 MVP 子集）

---

## 1. 背景与目标

CaPilot 目前的"项目"就是 `~/CaPilot/workspaces/<name>` 一个文件夹，所有 AI 会话共享这份代码。
我们希望引入 ORCA 式的工作树隔离：让用户基于一个 git 仓库开出**多个互不干扰的独立工作区**，
每个工作区落在独立分支上，共用同一份 `.git` 账本。AI 可以在里面大胆改，改坏了直接删掉工作区，主代码无损。

### 目标（MVP）

- 基于现有 git 仓库创建隔离工作区（`git worktree add`）
- 列出 / 移除工作区（`git worktree remove`，清理 git 元数据）
- 新工作区作为独立"项目"出现在左侧栏，带分支徽标
- 打开终端 / Git 面板复用现有机制，零改造即可工作
- 重启后能对账、恢复工作区记录

### 明确不做（后续版本）

- 稀疏检出（sparse checkout）
- 共享目录符号链接 / `.worktreeinclude` 拷贝
- 远程 SSH 工作区
- 谱系追踪（parent / lineage / 环检测）—— 第一版只记一个 `parent_id` 可选字段，不做链式逻辑
- 分支前缀配置（`git-username` / custom）—— 第一版固定无前缀

---

## 2. 核心概念（简版）

| 概念 | 说明 |
| --- | --- |
| 仓库 (repo) | 一个用 git 管理的文件夹。在 CaPilot 里即某个项目的 root（被克隆的文件夹 / 选中的文件夹 / `workspaces/<name>`）。 |
| 工作区 (worktree) | `git worktree add` 出来的独立文件夹，有自己独立的分支、index、HEAD，但共享 `.git`。 |
| 项目 (project) | CaPilot 的 UI 单元。工作区工作区 = 一个 root 指向 worktree 路径的「自定义根项目」。 |

**关键复用点：** CaPilot 已有「自定义根项目」机制（`create_project(name, path)` + `agent_spawn(project_root)`）。
工作区工作区本质就是一个 root = worktree 路径的项目，因此 PTY / Git 面板 / 会话持久化全部能复用，只需新增 worktree 的创建 / 注册 / 删除。

---

## 3. 数据模型

### 3.1 新增 SQLite 表 `worktrees`

`src-tauri/src/persistence.rs` 的 `SessionsDb::open` 中 `execute_batch` 增加建表：

```sql
CREATE TABLE IF NOT EXISTS worktrees (
    id            TEXT PRIMARY KEY,   -- 路径派生，如 <repoId>::<path>
    repo          TEXT NOT NULL,      -- 源仓库 root（绝对路径）
    path          TEXT NOT NULL,      -- worktree 文件夹绝对路径
    branch        TEXT NOT NULL,      -- 检出的分支名
    base_ref      TEXT,               -- 分叉基点（如 main / origin/main）
    parent_id     TEXT,               -- 可选父工作区（第一版仅存储，不校验链）
    instance_id   TEXT NOT NULL,      -- 每次创建 mint 的 UUID，复用路径不继承旧状态
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
```

### 3.2 Rust 结构体

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeMeta {
    pub id: String,
    pub repo: String,
    pub path: PathBuf,
    pub branch: String,
    pub base_ref: Option<String>,
    pub parent_id: Option<String>,
    pub instance_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}
```

### 3.3 存取方法（加到 `SessionsDb`）

- `insert_worktree(&WorktreeMeta)`
- `get_worktree(id)`
- `list_worktrees()`（可按 repo 过滤）
- `delete_worktree(id)`

对账需要"按 repo 列出"以便和 `git worktree list` 结果对比。

---

## 4. 后端实现

### 4.1 新模块 `src-tauri/src/worktree.rs`

git 命令全部通过 `git_gate` 走（路径白名单 + 并发门控）。

| 函数 | 执行的 git | 说明 |
| --- | --- | --- |
| `list_worktrees_in(repo)` | `git worktree list --porcelain` | 返回已有 worktree 的 path/branch，供查重与对账 |
| `add_worktree(repo, branch, path, base)` | `git worktree add --no-track -b <branch> <path> <base>` | 新建分支 + 独立工作区 |
| `add_worktree_existing(repo, branch, path)` | `git worktree add <path> <branch>` | 复用已有分支（校验 commit 一致且无其他 worktree 检出） |
| `remove_worktree(path)` | `git worktree remove <path>` | 通过 git 清理（绝不直接 `rm -rf`）|
| `resolve_default_branch(repo)` | `git symbolic-ref refs/remotes/origin/HEAD` → 失败则探测 `refs/heads/*` | 解析默认分叉基点 |
| `set_auto_setup_remote(repo)` | `git config push.autoSetupRemote true` | 让普通 push 自动建 `origin/<branch>` |

### 4.2 命名 / 净化（同模块）

- `sanitize_workspace_name(name) -> String`：非 `[A-Za-z0-9._-]` 字符替换为 `-`，去掉首尾 `-`/`.`
- `compute_worktree_path(repo_root, name, index) -> PathBuf`：
  - 默认放 `<repo_root>/../<repo_name>-<name>`（同级兄弟，避免塞进被 git 跟踪的目录）
  - 防路径穿越：净化后的 name 不得含 `/`、`.`、`..`；最终路径必须 `starts_with(repo_root.parent())`
- `validate_branch_name`：复用 `src-tauri/src/lib.rs:2138` 现成函数
- **候选名循环**：`name` → `name-2` → `name-3` …（最多 100 次），每次检查（a）分支是否已存在且被其他 worktree 占用，（b）路径是否可写

### 4.3 Tauri 命令（加到 `src-tauri/src/lib.rs`）

```rust
#[tauri::command]
async fn worktree_create(
    repo: String,                 // 源仓库 root
    name: String,                 // 工作区名
    base: Option<String>,         // 可选分叉基点
    parent_id: Option<String>,    // 可选父工作区
) -> Result<WorktreeMeta, String>;

#[tauri::command]
async fn worktree_list(repo: String) -> Result<Vec<WorktreeMeta>, String>;

#[tauri::command]
async fn worktree_remove(path: String) -> Result<(), String>;
```

创建流程（`worktree_create`）：
1. `git_gate::validate_repo(&repo)` 校验仓库路径
2. 净化 name → 分支名 / 文件夹名
3. 解析 base（默认分支探测）
4. 候选名循环调用 `add_worktree` / `add_worktree_existing`
5. `set_auto_setup_remote`
6. 铸造 `instance_id = UUID`，写入 DB（`insert_worktree`）
7. `create_project(name, worktree_path)` 建立项目壳（`project.json` root 指向 worktree 路径）
8. 返回 `WorktreeMeta`

### 4.4 `git_gate` 扩展

`allowed_roots()` 目前只允许 workspace_root 下的目录和 `custom_project_root`。
worktree 可能落在仓库同级（`<repo_root>/../<repo_name>-<name>`），不属于两者。
在 `git_gate::allowed_roots` 增加：扫描 DB `worktrees` 表的 `path` 一并加入白名单，
或让 `worktree.rs` 里的命令直接用已校验的绝对路径调用（绕开 `validate_repo`，但保留并发门控）。

> 方案：`git_gate` 增加 `run_raw(path, args)`（不做路径白名单，仅并发/速率门控），
> 由 `worktree.rs` 自行保证路径来自 `validate_repo` 派生的白名单目录。

---

## 5. 生命周期与事件

- **创建**：`worktree_create` 完成后 emit `worktree://created`（`WorktreeMeta`）。
  前端 `state/worktree.ts` 收到后：`addProject(meta.name, meta.path)`、刷新侧栏、可自动开终端。
- **移除**：`worktree_remove` 成功 → emit `worktree://removed` → 前端 `removeProject`。
- **启动对账**：`main.rs` 启动时调用 `worktree_reconcile`：
  - `git worktree list` 扫每个在册 repo
  - 发现 git 里有但 DB 没有 → 补登记
  - 发现 DB 有但 git 已删 → 从 DB 删除（标记孤儿）
- **失败兜底**：创建失败在命令层返回 `Err(String)`，前端弹错误并回滚已建的目录 / 项目壳。

---

## 6. 删除语义（重要，防坑）

现 `delete_project` 直接 `rm -rf` 项目目录 —— **对 worktree 工作区绝不能用**，否则 `.git` 会残留"这个 worktree 还在"的脏元数据，后续 git 命令报错。

必须新增专用路径：

```
移除 worktree 项目
  → 先 kill 其所有 agent 会话（复用现有）
  → 删除 DB sessions + worktrees 记录
  → 删除项目壳元数据（project.json 等）
  → git worktree remove <path>   ← 清理 git 元数据
  → (可选，后续) git branch -D <branch>
```

在左侧栏上下文菜单把「移除项目」与「移除工作区」分开：检测到该项目 root 是 DB 里在册的 worktree 时，走上面的专用流程。

---

## 7. 前端实现

### 7.1 新建项目弹窗 `LeftSidebar.tsx` 的 `NewProjectModal`

在「从 Git 克隆」下方加一节「从仓库创建隔离工作区」：
- 下拉/选择：当前哪个项目 root 是 git 仓库（可复用 `git_repo_info` 探测）
- 输入：工作区名称、可选 base 分支（默认自动探测）
- 提交：`invoke("worktree_create", {...})`

### 7.2 store 扩展（`ui/state/store.ts`）

- `worktrees: WorktreeMeta[]`
- `addWorktree(meta)` / `removeWorktree(path)`：保持与 `projects` / `projectRoots` 同步
- 现有 `addProject` / `removeProject` 复用

### 7.3 侧栏徽标

`LeftSidebar.tsx` 项目头 `pj-name` 旁：若该项目 root 命中在册 worktree，显示 `<分支名>` 徽标（`Icon name="git-branch"` 或文字 chip）。

### 7.4 右键菜单

项目上下文菜单新增「移除工作区」（仅当该 root 是 worktree 时显示），走 `worktree_remove` 专用删除流程。

### 7.5 Git 面板 / 终端

**零改动**。它们只认 `root = projectRoots[name]`（= worktree 路径）。
`RightSidebar.tsx` 的 `useProjectRoot()` 自动指向 worktree 文件夹，git 状态 / 分支 / 提交、PTY cwd 天然正确。

---

## 8. 测试计划

后端（`cargo test`，`src-tauri/src/worktree.rs` + `persistence.rs`）：
- `validate_branch_name` 复用现有测试
- 净化函数：`"My Feature!"` → `My-Feature`；含 `/`、`..` 被拒绝
- 候选名循环：`foo` 已占用 → 返回 `foo-2`
- base 解析：仓库有 `origin/HEAD` → 取之；无远程 → 回退本地默认分支
- DB 往返：`insert_worktree` / `list_worktrees` / `delete_worktree`
- 集成（在 temp 目录建真实 git 仓库）：`worktree_create` 后 `git worktree list` 能看到新分支 + 新目录；`worktree_remove` 后 git 元数据干净

前端：`pnpm tsc --noEmit` 类型检查 + 手动验收（新建 / 列表徽标 / 移除 / 重启对账）。

---

## 9. 实施顺序（里程碑）

| 里程碑 | 内容 | 依赖 |
| --- | --- | --- |
| **M1 — 后端核心** | `worktree.rs` 四个原语 + 净化/base 解析 + 候选循环 + 测试 | 无 |
| **M2 — 数据层** | `worktrees` 表 + `WorktreeMeta` + CRUD | M1 |
| **M3 — 命令与生命周期** | `worktree_create/list/remove` + 事件 + 启动对账 + 专用删除 | M2 |
| **M4 — 前端** | 弹窗 + store + 侧栏徽标 + 右键移除 | M3 |
| **M5 — 收尾** | 手动验收、错误兜底、README / RUNBOOK 更新 | M4 |

---

## 10. 风险与注意事项

- **删除语义**：worktree 项目绝不能走 `rm -rf`，必须 `git worktree remove`。
- **路径白名单**：worktree 路径在仓库同级，不在现有 `allowed_roots`；需 `run_raw` + 自行校验，避免打开任意路径执行 git 的漏洞面。
- **候选名 / 分支冲突**：同仓库多 worktree 并发创建时，查重需与 `git worktree list` 实时比对。
- **脏元数据残留**：应用强杀 / 手动删目录后，启动对账要能识别"git 里已不存在"的孤儿记录并清理。
- **base 分支解析失败**：无远程且无提交的空仓库无法探测默认分支，需给用户明确错误，建议先 clone 再建工作区。

---

## 附：相关文件

| 关注点 | 路径 |
| --- | --- |
| 后端新模块 | `src-tauri/src/worktree.rs`（新建）|
| 数据层 | `src-tauri/src/persistence.rs`（建表 + CRUD）|
| 命令注册 | `src-tauri/src/lib.rs`（`worktree_*`）|
| git 门控 | `src-tauri/src/git_gate.rs`（`run_raw`）|
| 启动对账 | `src-tauri/src/main.rs` |
| 前端弹窗 / 侧栏 / 右键 | `ui/components/layout/LeftSidebar.tsx` |
| store | `ui/state/store.ts` |
| 事件处理 | `ui/state/worktree.ts`（新建）|