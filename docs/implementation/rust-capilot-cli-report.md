# Rust capilot CLI（开发环境）实施报告

## 结论

Linux 开发环境中的 `capilot` 正式调用路径已从 Bash/Python shim 替换为 Rust CLI。执行一次 `pnpm tauri dev` 会自动构建 CLI，并让现有 Agent PATH 中的 `~/CaPilot/bin/capilot` 指向该 Rust debug binary，不需要用户安装或启动 Python。

本文最初记录 Rust CLI 替换。后续 Step 3 已在同一 CLI 上加入正式的 task-aware report；production bundle、sidecar、installer、release PATH、Windows IPC 仍不在当前范围。

## 历史实现与替换原因

此前项目通过 Tauri 启动时动态写入 `~/CaPilot/bin/capilot`，原因是不同 runtime 的 Agent 都是独立 CLI 子进程，安装目录和 shell 环境不统一；给所有 Agent 的 PATH 前置一个固定目录，便能稳定找到编排命令。

Step 2 一度把该文件改为 Bash 启动 Python 3，再由 Python 解析参数、序列化 JSON 和连接 Unix socket。仓库中还存在一个更早的 `scripts/capilot`，它会在缺少 socat/nc 时回退 Python。两者均已从仓库正式路径删除。

固定 PATH 机制本身仍然适合开发环境，因此本次保留 `~/CaPilot/bin` 注入，只替换其中 `capilot` 的来源。

## 当前开发链路

```text
pnpm tauri dev
  → package.json 的 tauri script 先执行 cargo build --bin capilot
  → 生成 src-tauri/target/debug/capilot
  → Tauri dev 启动 IDE
  → Rust install_dev_cli() 检查 debug binary
  → 更新 ~/CaPilot/bin/capilot 符号链接
  → Agent 启动时 PATH 前置 ~/CaPilot/bin
  → Master/Worker 直接执行 Rust capilot
  → Rust CLI 发送一行 serde_json 到 Dispatcher Unix socket
  → 原样输出 Dispatcher JSON 响应
```

现有 Agent PATH 注入函数仍位于 `src-tauri/src/lib.rs` 的 `capilot_path_env()`。每个 Agent PTY 创建时都会使用该环境，因此不需要修改 Claude Code、Codex、OpenCode 等 runtime adapter。

## 修改文件

### `src-tauri/src/bin/capilot.rs`

新增轻量 Rust CLI。它只承担边界职责：

- 手写解析 `status`、`dispatch`、`report` 与内部 `ping` 参数；没有引入 clap。
- 新 dispatch：`--worker`、可选 `--title`、`--prompt`。
- 保留旧 dispatch：`capilot dispatch <worker> <prompt...>`。
- report 现发送 `task_id + reporter_agent_id + status + result/error`；Task 生命周期判断仍全部由 Dispatcher 负责。
- 使用已有 `serde` / `serde_json` 生成请求。
- 从 `~/.capilot/socket` 读取 Dispatcher 实际 socket；也支持测试/诊断用 `CAPILOT_SOCKET` 覆盖。
- 使用 Rust 标准库 `UnixStream`，设置 5 秒读写超时。
- 输出 Dispatcher 返回的 JSON，不解释 Task、Worker 或 project 业务。

参数由操作系统 argv 传入，不经过 `$*`、shell 文本协议或 Python。因此引号由调用 shell 正常去除后作为参数内容保留，中文、空格和单个参数内的换行由 serde_json 安全编码。

### `src-tauri/src/orchestration/dev_cli.rs`

- 仅在 debug + Unix 构建中启用开发安装。
- 从正在运行的 Tauri debug executable 同目录定位 `target/debug/capilot`，不硬编码整个项目绝对路径。
- 创建 `~/CaPilot/bin`，将其中 `capilot` 更新为指向 Rust binary 的符号链接。
- 会替换先前生成的普通脚本文件或旧符号链接。
- 如果 CLI 未由 `pnpm tauri dev` 预先构建，会记录明确错误，不静默写回脚本。

### `package.json`

`tauri` script 改为：

```text
cargo build --manifest-path src-tauri/Cargo.toml --bin capilot && tauri
```

因此用户原来的命令 `pnpm tauri dev` 不变，但 pnpm 会把 `dev` 参数传给后半段 Tauri CLI，并在它之前确保 Rust CLI 已构建。

### `src-tauri/src/lib.rs`

- 启动时由 `install_dev_cli()` 替代 `install_shim()`。
- 保留 `~/CaPilot/bin` 的 Agent PATH 注入。
- 不再生成 Bash/Python 文件。

### 删除项

- 删除 `src-tauri/src/orchestration/shim.rs`。
- 删除历史 `scripts/capilot`。
- `orchestration/mod.rs` 改为导出 `dev_cli`，不再导出 `shim`。

### 文档修正

`docs/工作内容/step-2-report.md` 中关于 Python shim 的描述已更新为 Rust CLI，避免旧报告继续误导后续实现。

## 支持的命令

```bash
capilot status

capilot dispatch \
  --worker "阿比西尼亚" \
  --title "检查 README" \
  --prompt $'第一行\n第二行 "带引号"'

capilot dispatch 阿比西尼亚 "检查 README"

capilot report task_<完整ID> succeeded "结果摘要"
capilot report task_<完整ID> failed "错误摘要"
```

最后两条 report 命令会从 `CAPILOT_AGENT_ID` 自动取得报告者身份，并由 Dispatcher 完成持久化 Task。

## Step 2 行为保持情况

以下逻辑仍在原 Dispatcher 中，未被移动或弱化：

- `capilot status` 与 Master project scope。
- Worker 完整内部 ID、完整显示名、唯一 ID prefix 解析。
- NotFound/歧义结构化错误。
- 新旧 dispatch 格式。
- `task_<UUID>` 创建。
- SQLite queued → running。
- Worker Busy 与 `active_task_id` 绑定。
- Worker 自动收到包含完整 Task ID 的任务。
- PTY 写入失败时 Task failed、Worker Idle 回滚。
- Dispatcher 结构化 JSON 响应。

原 Step 2 Dispatcher 自动测试仍全部通过。

## 测试覆盖

Rust CLI 新增测试：

- 新 dispatch 保留中文、空格、双引号与多行 prompt。
- 旧 dispatch 兼容。
- report 的中文、多行内容、状态限制与 Agent 身份字段。
- 使用真实 Unix socket 完成 JSON 请求/响应交换。
- 开发安装能把已有普通脚本替换为 Rust binary 符号链接。

完整验证结果（2026-08-09）：

- `cargo test --bin capilot`：4 passed，0 failed。
- `cargo test`：库测试 57 passed，CLI 测试 4 passed，总计 61 passed，0 failed。
- `cargo check --all-targets`：通过；仅保留项目已有/后续 Step 预留 API 的 unused/dead-code 警告。
- `pnpm exec tsc --noEmit`：通过。
- `pnpm run build`：通过；只有已有的 Vite chunk size 提示。
- `git diff --check`：通过。
- 本次修改的 Rust 文件已执行 rustfmt。

## `pnpm tauri dev` 真实验证

已实际启动 `pnpm tauri dev`，观察到：

1. pnpm 首先执行并完成 `cargo build --bin capilot`。
2. Vite 与 `target/debug/capilot-ide` 正常启动。
3. 原 `~/CaPilot/bin/capilot` Bash 文件被替换为符号链接：
   `~/CaPilot/bin/capilot -> <项目>/src-tauri/target/debug/capilot`。
4. `file -L` 确认目标为 Linux ELF Rust executable，不是脚本。
5. Rust CLI 通过实际运行中的 Dispatcher socket 执行 `capilot status`，得到 `[]` JSON。
6. 使用中文、多行、空格和引号执行新格式 dispatch，实际 Dispatcher 返回结构化 `master_not_found`。该测试实例没有活动 Master，所以未创建真实 Task；但它确认参数、JSON、socket、peer credential 与 Dispatcher 响应全链路均工作且不经过 Python。

成功 dispatch、Task 创建、Busy 绑定和失败回滚继续由已通过的 Dispatcher 测试覆盖；完整有状态 UI 派发可按 Step 2 的既有验收步骤进行。

## Python 运行时依赖结论

当前 `capilot` 开发调用路径没有 Python 运行时依赖：

- 不生成 Python/Bash shim。
- 不调用 `python` / `python3`。
- 不保留 socat/nc/Python fallback。
- CLI 参数、JSON 和 Unix socket 全部由 Rust 完成。

仓库的正式源代码与开发脚本中也已清除原 shim 的 Python 调用。本结论仅针对本次要求的 Linux `pnpm tauri dev` 开发路径；生产打包问题刻意未在本次处理。
