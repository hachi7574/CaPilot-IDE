# CaPilot IDE — 隔离工作区（git worktree）实现工作日志

> **日期:** 2026-08-15
> **状态:** 全部完成（M1–M5）
> **计划:** `docs/reference/worktree-isolation-plan.md`
> **执行环境:** 隔离工作区 `.claude/worktrees/worktree-isolation`（分支 `worktree-worktree-isolation`）

---

## 里程碑进度

| 里程碑 | 状态 | 备注 |
| --- | --- | --- |
| M1 — 后端核心: worktree.rs 原语 + 净化/base解析/候选循环 + 测试 | ✅ 完成 | 13 个测试通过 |
| M2 — 数据层: worktrees 表 + WorktreeMeta + CRUD | ✅ 完成 | 13 个 persistence 测试通过 |
| M3 — 命令与生命周期: worktree_create/list/remove + 事件 + 启动对账 + 专用删除 | ✅ 完成 | 全量 158 个 lib 测试通过 |
| M4 — 前端: 弹窗 + store + 侧栏徽标 + 右键移除 | ✅ 完成 | tsc 干净 + 全量测试通过 |
| M5 — 收尾: 手动验收、错误兜底、文档更新 | ✅ 完成 | 全量校验通过；发现并修复 3 处兜底缺口 |

---

## 工作记录

### 2026-08-15 — 初始化

- 进入隔离工作区 `.claude/worktrees/worktree-isolation`，基线为 `297ca15f4`（与 main 的 HEAD 一致）。
- 通读计划文档与现有代码：
  - `src-tauri/src/persistence.rs`：`SessionsDb::open` 在 `execute_batch` 建表；已有 `ensure_column` 迁移辅助。
  - `src-tauri/src/git_gate.rs`：`validate_repo` 白名单 + 并发/速率门控；需加 `run_raw`。
  - `src-tauri/src/lib.rs`：`validate_branch_name`（2138 行）、`create_project` / `delete_project` / `git_clone`、事件 emit 模式、`run()` 启动流程。
  - `ui/components/layout/LeftSidebar.tsx`：`NewProjectModal`、项目头 `pj-name`、项目右键菜单（ctx.project）。
  - `ui/state/store.ts`：`projects` / `projectRoots` / `addProject` / `removeProject`。
  - `ui/App.tsx`：挂载 `useCloneEvents` 等事件 hook。

---

### M1 — 后端核心（✅ 完成）

**改动文件:**
- `src-tauri/src/worktree.rs`（新建）：git 原语 + 命名净化 + 候选循环。
  - 原语：`list_worktrees_in`（`git worktree list --porcelain` + 解析）、`add_worktree`（`git worktree add --no-track -b <branch> <path> [<base>]`）、`add_worktree_existing`（复用空闲已有分支）、`remove_worktree`（`git worktree remove [--force] <path>`，绝不 `rm -rf`）、`resolve_default_branch`（`origin/HEAD` → 当前分支 → 任意本地分支）、`set_auto_setup_remote`（`push.autoSetupRemote true`）。
  - 净化：`sanitize_workspace_name`（非 `[A-Za-z0-9._-]` → `-`，去首尾 `-`/`.`）、`validate_workspace_name`（拒绝空/`.`/`..`/`.`前缀/`/`）、`compute_worktree_path`（`<repo_parent>/<repo_name>-<name>` 同级兄弟 + `starts_with(parent)` 兜底）。
  - 候选循环：`create_worktree` 尝试 `name`/`name-2`/…（≤100），实时查 `git worktree list`；分支空闲已存在 → 复用；被占用/路径已存在/git 失败 → 下一个。显式 base 先 `rev-parse --verify` 校验，不存在则快速报错。成功后 `set_auto_setup_remote`。
- `src-tauri/src/git_gate.rs`：新增 `run_raw(path, args)`（保留并发/速率门控，不做路径白名单；用于 worktree 目标路径在允许根之外）。
- `src-tauri/src/lib.rs`：`mod worktree;` 注册；`validate_branch_name` 改为 `pub(crate)` 供 worktree 模块复用。

**测试:** `cargo test worktree::` — 13 个测试通过（净化、路径越界拒绝、porcelain 解析、真实 git 仓库集成：建/复用/后缀/显式 base 校验/移除后 git 元数据干净）。

**当前状态:** M1 的原语在 M3 接线前会有 dead_code 警告，属预期。

### M2 — 数据层（✅ 完成）

**改动文件:** `src-tauri/src/persistence.rs`

- 新增 `WorktreeMeta` 结构体（`id`/`repo`/`path`/`branch`/`base_ref`/`parent_id`/`instance_id`/`created_at`/`updated_at`）+ `worktree_id(repo, path)` 派生 `id = "<repo>::<path>"`。
- `SessionsDb::open` 的 `execute_batch` 增加 `worktrees` 建表（与计划 §3.1 一致）。
- `SessionsDb` 新增 CRUD：`insert_worktree`（upsert）、`get_worktree`、`find_worktree_by_path`（供按路径删除/对账）、`list_worktrees`、`list_worktrees_for_repo`、`delete_worktree`、`delete_worktrees_for_repo`。

**测试:** `cargo test persistence::` — 13 个通过（含新增 `worktree_crud_roundtrip`：往返、按路径查、按 repo 过滤、单删、整 repo 删）。

### M3 — 命令与生命周期（✅ 完成）

**改动文件:** `src-tauri/src/lib.rs`

- 新增 Tauri 命令并注册进 `invoke_handler`：
  - `worktree_create(repo, name, base, parent_id) -> WorktreeMeta`：校验源仓库 → `create_worktree`（净化/base/候选循环）→ mint `instance_id` → `insert_worktree` → `unique_project_name` 去重项目名 → `create_project(name, path)` 建项目壳 → emit `worktree://created`。任一步失败回滚已建 worktree 目录 + DB 行。
  - `worktree_list(repo) -> Vec<WorktreeMeta>`：按源仓库过滤 DB。
  - `worktree_remove(path)`：专用删除流程（§6）——kill 该项目的所有 agent 会话 → 删 sessions + worktrees DB 行 → `delete_project_dir` 删项目壳 → `git worktree remove --force` → `git worktree prune` 清理脏元数据 → emit `worktree://removed`。
- `worktree_reconcile(&Persistence)`：启动对账——DB 有但 git 无/目录已删 → 删孤儿行；git 有但 DB 无（排除主工作区）→ 补登记（新 `instance_id`）。在 `run()` 的 `.setup()` 中 spawn 到后台线程执行。
- `delete_project` 增加护栏：项目 root 命中在册 worktree 时拒绝，引导走「移除工作区」。
- 事件 payload：`worktree://created` = `{ meta, name }`（name 为 CaPilot 项目名，可能带 `-N` 后缀），`worktree://removed` = `{ path, name }`。
- 辅助：`unique_project_name`（项目壳名去重，避免与源项目同名覆盖 project.json）、`project_name_for_root`（按 root 反查项目壳名）。

**测试:** 新增 3 个（`unique_project_name_avoids_existing_project_dirs`、`project_name_for_root_finds_the_matching_shell`、`worktree_reconcile_drops_orphan_and_adopts_external`）。全量 `cargo test`：158 lib + 3 daemon_smoke 全通过。

### M4 — 前端（✅ 完成）

**改动文件:** `ui/state/store.ts`、`ui/state/worktree.ts`（新建）、`ui/App.tsx`、`ui/components/layout/LeftSidebar.tsx`、`ui/App.css`、`src-tauri/src/lib.rs`

- `ui/state/store.ts`：
  - 新增 `WorktreeMeta` TS 接口（与 Rust 序列化 snake_case 一致：`base_ref`/`parent_id`/`instance_id`/`created_at`/`updated_at`）。
  - `AppState` 新增 `worktrees: WorktreeMeta[]` 状态 + `setWorktrees` / `addWorktree` / `removeWorktreeLocal` / `removeWorktree` 动作。
  - store creator 从对象字面量改块体，抽出共享 `dropProjectLocal(name)`（仅本地清理：列表/root 映射/焦点/tab/agent，不发后端调用）；`removeProject` 复用它并保留 `delete_project`；`removeWorktreeLocal` 供 `worktree://removed` 事件用（后端已完成删除，前端不再二次调用）。
  - `addWorktree(meta, name)` 幂等登记 worktree + `addProject` 挂出项目壳。
- `ui/state/worktree.ts`（新建）：`useWorktreeEvents()` 遵循 `clone.ts` 的 StrictMode 双挂载守卫模式——挂载时 `worktree_list_all` 拉全量注册表（重启后徽标可恢复）；监听 `worktree://created` → `addWorktree` + `setFocusedProject` + 自动 `spawnAgent`；`worktree://removed` → `removeWorktreeLocal(path)`。
- `ui/App.tsx`：挂载 `useWorktreeEvents()`。
- `ui/components/layout/LeftSidebar.tsx`：
  - 项目头 `pj-name` 旁：root 命中在册 worktree → 显示 `<分支名>` 徽标（`.wt-badge`，`git-branch` 图标）。
  - 项目右键菜单：worktree 项目显示「移除工作区」（danger，走 `removeWorktree(wt.path)`）；普通项目仍显示「移除项目」。判定用 `projectRoots[proj] ?? ctx.cwd` 匹配 `wt.path`。
  - `NewProjectModal` 新增「从仓库创建隔离工作区」区块：挂载时对每个项目 root 调 `git_repo_info` 过滤出 git 仓库 → `<select>` 源仓库 + 工作区名称（可空）+ 基础分支（可空）→ `invoke("worktree_create", { repo, name, base, parentId })`。成功靠 `worktree://created` 事件收尾（注册 + 聚焦 + 自动开终端），错误经 `nprojError` 展示并保持弹窗打开。
- `ui/App.css`：`.wt-badge`（静态徽标，max-width 45% 防溢出）、`.wt-repo-select`（下拉框配色）。
- `src-tauri/src/lib.rs`：新增 `worktree_list_all() -> Vec<WorktreeMeta>` 命令（全量注册表，供前端挂载恢复徽标）并注册进 `invoke_handler`。

**验证:** `pnpm tsc --noEmit` 干净通过；`cargo check --lib` 干净；全量 `cargo test` 158 lib + 3 daemon_smoke 通过。UI 自动化在本机 Wayland 受限，前端改动以 tsc + 代码走读验证（`list_projects` 的 root 来自 project.json 的 canonical 路径，与 worktree DB 的 canonical path 一致，前后端匹配成立）。

### M5 — 收尾（✅ 完成）

**改动文件:** `ui/components/layout/LeftSidebar.tsx`、`ui/App.css`、`ui/state/store.ts`

**手动验收（本机 Wayland 下 UI 自动化受限，按记忆指引以「代码走读 + CLI」替代）：**
- 全量校验：`pnpm tsc --noEmit` 干净；`cargo test` 158 lib + 3 daemon_smoke 全通过；`cargo check --lib` 干净。
- 走读完整链路：创建弹窗「从仓库创建隔离工作区」→ `worktree_create`（净化/base/候选循环 → 项目壳 → emit `worktree://created`）→ 事件 hook `addWorktree` + `setFocusedProject` + 自动开终端 → 侧栏 `pj-name` 旁 `<分支名>` 徽标（root 匹配 worktree DB 的 canonical path）→ 右键「移除工作区」→ `worktree_remove`（kill 会话 → 删 DB 行 + 项目壳 → `git worktree remove --force` → `prune` → emit `worktree://removed`）→ 事件 hook `removeWorktreeLocal` 幂等收尾。
- 确认启动对账（`.setup()` spawn `worktree_reconcile`）仍在位；`delete_project` 对在册 worktree 的护栏仍在位；`list_worktrees()`（全量）已被 M2 CRUD 测试覆盖，支撑 `worktree_list_all`。

**错误兜底（M5 发现并修复）：**
- 弹窗工作区名称：后端净化后为空会报「工作区名称净化后为空」→ 占位符改「必填」，按钮在名称为空时禁用，避免误导性「可空 = 自动生成」。
- agent 右键菜单「关闭并移除项目」（项目只剩最后一个终端时）此前直接走 `removeProject`，对 worktree 项目会被后端护栏拒绝 → 本地消失但磁盘/DB 残留。修复：worktree 项目改走 `removeWorktree`（文案变「关闭并移除工作区」）。
- 弹窗源仓库扫描会把已登记工作区也列为源仓库（从工作区再 fork 会在共享 .git 上造出 `<worktreeDir>-<name>` 怪异命名）→ 扫描时按在册 worktree 过滤掉。
- CSS：`.wt-badge`/`.wt-repo-select` 最初引用了未定义的 `--border1`/`--bg1` → 改用已定义变量（`--bg2`/`--bg4`/`--ink2`），`<select>` 复用 `.nproj-input` 主题。
- `WorktreeMeta` TS 注释修正（实际 wire 为 snake_case，非 camelCase）。

**收尾结论:** 计划五里程碑全部完成。改动未提交（按要求只改不提交）。遗留：GUI 实机手测需在可交互环境进行（本环境 Wayland 无法注入输入/截屏）；`cargo clippy` 仍有 29 条既有警告，均在未触碰的旧代码（persistence rename_project 前缀改写、lib.rs `let _app`/`single_match`），worktree 相关文件无 clippy 警告。

---

（计划五里程碑全部完成）
