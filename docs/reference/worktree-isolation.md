# Worktree isolation and creation

This page describes how Orca creates an isolated workspace. The building block is a
git worktree (shared `.git` object store, isolated working tree / index / HEAD), and
Orca layers lineage tracking, path and branch naming, sparse checkout, shared/copied
directories, and workspace-scoped terminals on top.

Some mechanics are skipped on remote (SSH) hosts; the remote path deliberately avoids
the desktop's absolute `workspaceDir` and does not run the symlink/shared/copy steps
(they are local-only until the relay gains a method for them).

## End-to-end flow

```
orca worktree create (src/cli/handlers/worktree.ts:198)
  │  resolves repo / parent / cwd / env lineage, sanitizes names
  ▼
client.call('worktree.create', ...)  ── Unix socket JSON-RPC ──►
src/main/runtime/runtime-rpc.ts:429  ──►  rpc/methods/worktree.ts:97
  ▼
runtime.dedupeWorktreeCreate(...) ──► runtime.createManagedWorktree (orca-runtime.ts:21674)
  │  ├─ isFolderRepo?  ──► createFolderWorkspace (metadata only, no git)
  │  ├─ repo.connectionId? ──► createRemoteWorktree (worktree-remote.ts:1484)
  │  └─ local: base resolve → suffix-candidate loop → git worktree add → register
  ▼
returns CreateWorktreeResult (setup / startupTerminal / lineage / warnings)
```

### Phase 1 — CLI handler

`src/cli/handlers/worktree.ts:198` collects repo (`--project`/`--repo`, or inferred
from the current worktree selector via `getRepoSelectorFromWorktreeSelector`, line 137),
name, base branch, issue link, comment, and lineage inputs, then makes a single
`worktree.create` RPC call. `--parent-worktree` is mapped to either a workspace key
(folder parent) or a worktree selector in
`src/cli/handlers/worktree-create-parent-selector.ts`. `ORCA_WORKTREE_ID` /
`ORCA_WORKSPACE_ID` env vars are read as `envParentWorkspace`.

### Phase 2 — RPC dispatch

The method handler in `src/main/runtime/rpc/methods/worktree.ts:96` wraps the call in
`runtime.dedupeWorktreeCreate` (orca-runtime.ts:25961), an idempotency guard keyed on
`repoSelector + clientMutationId` with a 60 s result TTL, then calls
`runtime.createManagedWorktree`. The renderer's equivalent desktop path is the Electron
IPC `worktrees:create` in `src/main/ipc/worktrees.ts:2186`.

### Phase 3 — Runtime creation

`createManagedWorktree` (orca-runtime.ts:21674) branches on repo type:

- **Folder repo** — metadata-only folder workspace; no git worktree at all.
- **Remote** — delegates to `createRemoteWorktree` (worktree-remote.ts:1484) → the SSH
  provider's `git.addWorktree` (ssh-git-provider.ts:716) → relay
  `addWorktreeOp` (src/relay/git-handler-worktree-ops.ts:31).
- **Local git worktree** — the full sequence below.

### Phase 4 — The git call

`src/main/git/worktree.ts`:

- `addWorktree` (line 912): new branch runs
  `git worktree add --no-track -b <branch> <path> [<base-ref>]`; an existing branch
  reuses `git worktree add <path> <branch>`. `--no-track` avoids inheriting the base's
  upstream; `push.autoSetupRemote=true` is set (lines 1004–1026) so a plain `git push`
  still creates `origin/<branch>`.
- `addSparseWorktree` (line 1033): creates with `--no-checkout`, then
  `sparse-checkout init --cone` + `sparse-checkout set -- <dirs>` + `checkout <branch>`.
  On failure it rolls back with `removeWorktree(..., forceBranchDelete: true)`.
- Base refs are resolved by `resolveWorktreeAddBaseRef`
  (`src/shared/worktree-base-ref.ts:3`): short names resolve to `refs/remotes/<base>`
  first, then `refs/heads/<base>`, preferring the remote display name.
- Candidate name loop (orca-runtime.ts:22014): up to 100 attempts of `name`,
  `name-2`, … checking branch conflicts (`getBranchConflictKind`, git/repo.ts:992) and
  path availability.
- A base remote-tracking refresh may run first (`getOrStartRemoteTrackingBaseRefresh`);
  elsewhere `fetchRemoteWithCache` is the fallback.

### Phase 5 — Path and branch naming

- Path: `computeWorktreePath` (`src/main/ipc/worktree-logic.ts:100`) is
  `<workspaceRoot>/<sanitizedName>` or, when `nestWorkspaces` is set,
  `<workspaceRoot>/<repoName-without-.git>/<sanitizedName>`. WSL repos are forced onto
  the distro filesystem under `~/orca/workspaces` (lines 115–128). Remote workspaces
  fall back to sibling paths `<repoPath>/../<repoName>-<sanitizedName>` unless the repo
  defines its own `worktreeBasePath` (line 130). `ensurePathWithinWorkspace` (line 78) is
  the anti-path-traversal guard.
- Branch: `computeValidatedBranchName` (`src/main/ipc/worktree-branch-name.ts:44`) joins
  a configured prefix (`git-username` / `custom` / `none`, `src/shared/branch-prefix.ts`)
  with the sanitized name.
- Existing-branch reuse (`canCheckoutExistingLocalBranch`, orca-runtime.ts:2283): only
  when `refs/heads/<branch>` points at the same commit as the base **and** no other
  worktree has it checked out.

### Phase 6 — Lineage

`resolveLineageForWorktreeCreate` (orca-runtime.ts:28388) picks a parent in priority
order: explicit `--parent-worktree` → `--parent-workspace` → env workspace →
orchestration context → orchestration task id in the comment → caller terminal handle →
cwd. Conflicting candidates are refused with `LINEAGE_PARENT_CONTEXT_CONFLICT`.
`--no-parent` (or none) yields an independent root workspace.

`recordCreatedWorktreeLineage` (orca-runtime.ts:21331) persists two records — a
worktree-level lineage and a unified workspace-level lineage that also covers folder
workspaces (`workspaceWorkspaceKey`). Worktree ids are path-derived
(`repo.id::path`, line 22337) and every create mints a fresh `instanceId = randomUUID()`
so a reused path cannot inherit stale lineage. Parent chains are cycle-checked.

"Branch inheritance" is base selection, not a copy of the parent's branch:
`resolveWorktreeCreateBase` (`src/main/worktree-create-base.ts:8`): explicit
`--base-branch` wins, then `repo.worktreeBaseRef`, then the repo default-ref probe
(`getDefaultBaseRef`, git/repo.ts:513). The chosen base is persisted on the new branch
as `branch.<branch>.base` (`persistWorktreeCreationBase`, git/worktree.ts:320).

### Phase 7 — Registration and lifecycle

After `git worktree add`, the runtime re-lists worktrees and finds the created row by
branch/path (`findCreatedWorktree`, `src/main/ipc/created-worktree-reconciliation.ts:3`),
persists metadata (`store.setWorktreeMeta`), registers the root with the filesystem-auth
layer (`invalidateAuthorizedRootsCache`), records lineage, invalidates caches,
`notifyWorktreesChanged(repo.id)`, and emits a `worktree.created` lifecycle event
(main/index.ts:373). Then it runs the created worktree's own `orca.yaml` hooks
(`loadHooks(worktreePath)`, line 22458), schedules default tabs, and provisions a PTY
(`createTerminal(\`id:${worktree.id}\`)`, cwd = worktree path) plus the
`markLocalWorkspaceTrustedForAgent` trust markers (line 21297).

## Isolation mechanics beyond plain worktrees

| Mechanic | Implementation | Notes |
| --- | --- | --- |
| Sparse checkout | `addSparseWorktree` (git/worktree.ts:1033) | Only configured directories are materialized; surfaced as the `sparse` porcelain field on listing. |
| Shared heavy directories | `createWorktreeLinkedPaths` (`src/main/ipc/worktree-symlinks.ts:309`) from `repo.symlinkPaths` | Symlink (or APFS clone-copy) per worktree; removed on delete. |
| `orca.yaml worktree.sharedDirectories` | `resolveWorktreeSharedDirectories` (`src/main/git/worktree-shared-directories.ts:68`) → `createWorktreeSharedPaths` | Only directories that exist **and are gitignored** are symlinked, so no spurious git diffs. |
| `.worktreeinclude` | `resolveWorktreeIncludePaths` (`src/main/git/worktree-include-file.ts:76`) → `createWorktreeCopiedPaths` | **Copied**, never symlinked — each worktree owns its files (copy-budgeted by `src/main/ipc/worktree-include-copy-budget.ts`). |
| Git config | `branch.<branch>.base`, `push.autoSetupRemote=true` | Fork/PR review workspaces additionally get an upstream via `configureCreatedWorktreePushTarget`. |
| Workspace-scoped terminals | `createTerminal` scoped to `id:${worktree.id}` | PTY cwd and agent-trust markers are confined to the new path. |
| Remote isolation | `createRemoteWorktree` (worktree-remote.ts:1484) | No symlink/shared/copy steps; paths never reuse the desktop's absolute workspace dir. |

## Where the pieces live

| Concern | Path |
| --- | --- |
| CLI handler / parent selector | `src/cli/handlers/worktree.ts:198`, `src/cli/handlers/worktree-create-parent-selector.ts` |
| RPC method / schema | `src/main/runtime/rpc/methods/worktree.ts:96`, `worktree-schemas.ts:100` |
| Runtime create | `src/main/runtime/orca-runtime.ts:21674` (lineage resolve :28388, persist :21331) |
| Desktop IPC create | `src/main/ipc/worktrees.ts:2186` (folder workspace :1030) |
| Git `worktree add` | `src/main/git/worktree.ts:912` (`addWorktree`), `:1033` (`addSparseWorktree`) |
| SSH provider / relay parity | `src/main/providers/ssh-git-provider.ts:716`, `src/relay/git-handler-worktree-ops.ts:31` |
| Path / branch naming | `src/main/ipc/worktree-logic.ts:100`, `worktree-branch-name.ts:44`, `src/shared/branch-prefix.ts` |
| Base-ref resolution | `src/main/worktree-create-base.ts:8`, `src/shared/worktree-base-ref.ts:3` |
| Isolation extras | `src/main/ipc/worktree-symlinks.ts`, `src/main/git/worktree-shared-directories.ts:68`, `src/main/git/worktree-include-file.ts:76` |