# 工作树隔离与创建（Worktree Isolation and Creation）

本文梳理 Orca 如何创建隔离工作区。基础构件是 git worktree（共享 `.git` 对象库、隔离的工作树 / index / HEAD），Orca 在其上叠加了谱系跟踪、路径与分支命名、稀疏检出、共享/复制目录、以及工作区级终端。

远程（SSH）主机上会跳过部分机制：远程路径刻意规避桌面的绝对 `workspaceDir`，且不执行符号链接/共享/复制步骤（在 relay 获得对应方法之前仅限本地）。

## 端到端流程

```
orca worktree create (src/cli/handlers/worktree.ts:198)
  │  解析 repo / parent / cwd / env 谱系、净化命名
  ▼
client.call('worktree.create', ...)  ── Unix socket JSON-RPC ──►
src/main/runtime/runtime-rpc.ts:429  ──►  rpc/methods/worktree.ts:97
  ▼
runtime.dedupeWorktreeCreate(...) ──► runtime.createManagedWorktree (orca-runtime.ts:21674)
  │  ├─ isFolderRepo?  ──► createFolderWorkspace（仅元数据，无 git）
  │  ├─ repo.connectionId? ──► createRemoteWorktree (worktree-remote.ts:1484)
  │  └─ 本地: base 解析 → 后缀候选循环 → git worktree add → 注册
  ▼
返回 CreateWorktreeResult（setup / startupTerminal / lineage / warnings）
```

### 阶段 1 — CLI handler

`src/cli/handlers/worktree.ts:198` 收集 repo（`--project`/`--repo`，或通过 `getRepoSelectorFromWorktreeSelector`（137 行）从当前 worktree 选择器推断）、name、base 分支、issue 链接、comment 与谱系输入，然后发起一次 `worktree.create` RPC 调用。`--parent-worktree` 映射为工作区键（文件夹父级）或 worktree 选择器（`src/cli/handlers/worktree-create-parent-selector.ts`）。`ORCA_WORKTREE_ID` / `ORCA_WORKSPACE_ID` 环境变量作为 `envParentWorkspace` 读取。

### 阶段 2 — RPC 分发

`src/main/runtime/rpc/methods/worktree.ts:96` 中的方法 handler 用 `runtime.dedupeWorktreeCreate`（orca-runtime.ts:25961）包裹调用——以 `repoSelector + clientMutationId` 为键的幂等守卫，结果 TTL 60s——再调用 `runtime.createManagedWorktree`。渲染进程对应的桌面路径是 Electron IPC `worktrees:create`（`src/main/ipc/worktrees.ts:2186`）。

### 阶段 3 — 运行时创建

`createManagedWorktree`（orca-runtime.ts:21674）按 repo 类型分支：

- **文件夹 repo** —— 仅元数据的文件夹工作区，完全不建 git worktree。
- **远程** —— 委托 `createRemoteWorktree`（worktree-remote.ts:1484）→ SSH provider 的 `git.addWorktree`（ssh-git-provider.ts:716）→ relay `addWorktreeOp`（src/relay/git-handler-worktree-ops.ts:31）。
- **本地 git worktree** —— 走下面完整流程。

### 阶段 4 — Git 调用

`src/main/git/worktree.ts`：

- `addWorktree`（912 行）：新分支执行 `git worktree add --no-track -b <branch> <path> [<base-ref>]`；已有分支复用 `git worktree add <path> <branch>`。`--no-track` 避免继承 base 的 upstream；设置 `push.autoSetupRemote=true`（1004–1026 行），使普通 `git push` 也能创建 `origin/<branch>`。
- `addSparseWorktree`（1033 行）：用 `--no-checkout` 创建，随后 `sparse-checkout init --cone` + `sparse-checkout set -- <dirs>` + `checkout <branch>`。失败时用 `removeWorktree(..., forceBranchDelete: true)` 回滚。
- Base ref 由 `resolveWorktreeAddBaseRef`（`src/shared/worktree-base-ref.ts:3`）解析：短名先解析为 `refs/remotes/<base>`，再试 `refs/heads/<base>`，优先远程展示名。
- 候选名循环（orca-runtime.ts:22014）：最多 100 次尝试 `name`、`name-2`……逐一检查分支冲突（`getBranchConflictKind`，git/repo.ts:992）与路径可用性。
- 可能先跑一次 base 远程跟踪刷新（`getOrStartRemoteTrackingBaseRefresh`）；别处用 `fetchRemoteWithCache` 兜底。

### 阶段 5 — 路径与分支命名

- 路径：`computeWorktreePath`（`src/main/ipc/worktree-logic.ts:100`）为 `<workspaceRoot>/<sanitizedName>`，或当 `nestWorkspaces` 开启时是 `<workspaceRoot>/<repoName-without-.git>/<sanitizedName>`。WSL repo 被强制放到发行版文件系统 `~/orca/workspaces`（115–128 行）。远程工作区回退到同级路径 `<repoPath>/../<repoName>-<sanitizedName>`，除非 repo 定义了自有 `worktreeBasePath`（130 行）。`ensurePathWithinWorkspace`（78 行）是防路径穿越守卫。
- 分支：`computeValidatedBranchName`（`src/main/ipc/worktree-branch-name.ts:44`）把配置的前缀（`git-username` / `custom` / `none`，`src/shared/branch-prefix.ts`）与净化后的名字拼接。
- 已有分支复用（`canCheckoutExistingLocalBranch`，orca-runtime.ts:2283）：仅当 `refs/heads/<branch>` 指向与 base 相同 commit **且** 没有其他 worktree 在检出它时才允许。

### 阶段 6 — 谱系

`resolveLineageForWorktreeCreate`（orca-runtime.ts:28388）按优先级选父级：显式 `--parent-worktree` → `--parent-workspace` → env 工作区 → 编排上下文 → comment 中的编排任务 id → 调用方终端句柄 → cwd。冲突候选被拒并抛 `LINEAGE_PARENT_CONTEXT_CONFLICT`。`--no-parent`（或未指定）得到独立的根工作区。

`recordCreatedWorktreeLineage`（orca-runtime.ts:21331）持久化两条记录——worktree 级谱系 + 同时覆盖文件夹工作区的统一工作区级谱系（`workspaceWorkspaceKey`）。Worktree id 由路径派生（`repo.id::path`，22337 行），每次创建都会铸造全新的 `instanceId = randomUUID()`，因此复用路径不会继承过期的谱系。父链做了环检测。

"分支继承"其实是 **base 选择**，不是拷贝父分支：`resolveWorktreeCreateBase`（`src/main/worktree-create-base.ts:8`）：显式 `--base-branch` 优先，其次 `repo.worktreeBaseRef`，再其次 repo 默认-ref 探测（`getDefaultBaseRef`，git/repo.ts:513）。选定的 base 以 `branch.<branch>.base` 持久化到新分支上（`persistWorktreeCreationBase`，git/worktree.ts:320）。

### 阶段 7 — 注册与生命周期

在 `git worktree add` 之后，运行时重新列出 worktrees，按分支/路径找到新创建的行（`findCreatedWorktree`，`src/main/ipc/created-worktree-reconciliation.ts:3`），持久化元数据（`store.setWorktreeMeta`），向文件系统认证层注册 root（`invalidateAuthorizedRootsCache`），记录谱系，使缓存失效，`notifyWorktreesChanged(repo.id)`，并发出 `worktree.created` 生命周期事件（main/index.ts:373）。随后运行新 worktree 自己的 `orca.yaml` 钩子（`loadHooks(worktreePath)`，22458 行），调度默认标签页，并提供一个 PTY（`createTerminal(\`id:${worktree.id}\`)`，cwd = worktree 路径），外加 `markLocalWorkspaceTrustedForAgent` 信任标记（21297 行）。

## 超越普通 worktree 的隔离机制

| 机制 | 实现 | 说明 |
| --- | --- | --- |
| 稀疏检出 | `addSparseWorktree`（git/worktree.ts:1033） | 只物化配置的目录；以 listing 上的 `sparse` porcelain 字段暴露。 |
| 共享大目录 | `createWorktreeLinkedPaths`（`src/main/ipc/worktree-symlinks.ts:309`），来自 `repo.symlinkPaths` | 每个 worktree 建符号链接（或 APFS clone-copy）；删除时移除。 |
| `orca.yaml worktree.sharedDirectories` | `resolveWorktreeSharedDirectories`（`src/main/git/worktree-shared-directories.ts:68`）→ `createWorktreeSharedPaths` | 只有**存在且被 gitignore** 的目录才做符号链接，避免产生无关 git diff。 |
| `.worktreeinclude` | `resolveWorktreeIncludePaths`（`src/main/git/worktree-include-file.ts:76`）→ `createWorktreeCopiedPaths` | **拷贝**，绝不符号链接——每个 worktree 拥有自己的文件（由 `src/main/ipc/worktree-include-copy-budget.ts` 预算控制拷贝量）。 |
| Git 配置 | `branch.<branch>.base`、`push.autoSetupRemote=true` | Fork/PR 评审工作区另通过 `configureCreatedWorktreePushTarget` 获得 upstream。 |
| 工作区级终端 | `createTerminal` 限定到 `id:${worktree.id}` | PTY cwd 与 agent 信任标记被限制在新路径内。 |
| 远程隔离 | `createRemoteWorktree`（worktree-remote.ts:1484） | 无符号链接/共享/拷贝步骤；路径绝不复用桌面的绝对工作区目录。 |

## 各环节所在位置

| 关注点 | 路径 |
| --- | --- |
| CLI handler / parent 选择器 | `src/cli/handlers/worktree.ts:198`、`src/cli/handlers/worktree-create-parent-selector.ts` |
| RPC 方法 / schema | `src/main/runtime/rpc/methods/worktree.ts:96`、`worktree-schemas.ts:100` |
| 运行时创建 | `src/main/runtime/orca-runtime.ts:21674`（谱系解析 :28388、持久化 :21331） |
| 桌面 IPC 创建 | `src/main/ipc/worktrees.ts:2186`（文件夹工作区 :1030） |
| Git `worktree add` | `src/main/git/worktree.ts:912`（`addWorktree`）、`:1033`（`addSparseWorktree`） |
| SSH provider / relay 一致性 | `src/main/providers/ssh-git-provider.ts:716`、`src/relay/git-handler-worktree-ops.ts:31` |
| 路径 / 分支命名 | `src/main/ipc/worktree-logic.ts:100`、`worktree-branch-name.ts:44`、`src/shared/branch-prefix.ts` |
| Base-ref 解析 | `src/main/worktree-create-base.ts:8`、`src/shared/worktree-base-ref.ts:3` |
| 隔离附加项 | `src/main/ipc/worktree-symlinks.ts`、`src/main/git/worktree-shared-directories.ts:68`、`src/main/git/worktree-include-file.ts:76` |