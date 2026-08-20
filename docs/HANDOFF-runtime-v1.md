# 交接：Agent runtime v1 加速 + 画布结论

工作区：`/home/hachi/orca/workspaces/CaPilot-Ide/runtime_skill`  
分支：`hachi7574/runtime_skill`  
来源会话：分析 CaPilot 画布 / cleancode 对照、runtime 新增瓶颈、OpenCode PTY 显示、以及 v1 落地。

本文只交接**本工作区应继续的 runtime 工作**。主仓 `~/Project/CaPilot-Ide` 里其它未提交改动（release、theme-lab 等）**不要**并进本分支。

---

## 1. 目标（已拍板）

加快新增 agent runtime 的速度：

- **v1 完成定义**：PATH 上能探测 + 项目 `+` 能开 PTY tab + Composer 当普通输入框（明文 + Enter）。
- **明确砍掉**：Composer 按键剧本、status hook、resume_key、context usage、SGR 鼠标、resident xterm、slash 目录。
- 未知 runtime id **禁止** fallback 成 Claude。

Framebuffer TUI（OpenCode / Claude 全屏刷新）v1 也只给全屏 tab，**不进画布节点**。

---

## 2. 本工作区已改代码

| 文件 | 改动 |
| --- | --- |
| `src-tauri/src/agent_runtime/runtimes/generic.rs` | **新增** `GenericCliAdapter`：按 id 同名二进制 spawn；可 `with_binary(id, bin, extra_args)` |
| `src-tauri/src/agent_runtime/runtimes/mod.rs` | `pub mod generic`；`claude` 显式匹配；`other => GenericCliAdapter::from_id(other)` |
| `ui/state/store.ts` | `TermTemplate.runtime` 从字面量 union 改为 `string` |

单元测试：`cargo test --lib agent_runtime::runtimes::generic`（源仓已过）。本工作区若尚未编过，先编一次确认。

### 再加一个 CLI（v1 清单）

1. `known_runtimes()` 加上 id（`mod.rs`）。
2. `ui/state/store.ts` 的 `DEFAULT_TEMPLATES` 加一行 `{ id, name, command: "", runtime }`。
3. 若 argv 不是光秃二进制：`get_adapter` 里用 `GenericCliAdapter::with_binary(...)`，不要新开 700 行 adapter。

可选：`Icon.tsx` 图标、Settings `DEFAULT_LAUNCH` 预填。不要改 `Composer.tsx`。

Skill 清单：`.agents/skills/capilot-add-runtime/references/v1-checklist.md`。

---

## 3. Skill

已写入本仓：`.agents/skills/capilot-add-runtime/`

个人 Codex 目录另有一份（会话里创建的）：`~/.codex/skills/capilot-add-runtime/`  
两份应保持同文。本仓是工作区事实来源；若只在本 worktree 协作，以 `.agents/skills` 为准。

触发：用户说加 gemini / cursor / copilot / grok / 新 CLI harness。  
不要用这份 skill 去做画布、Git、或 v2 Composer TUI 自动化。

---

## 4. 会话结论（供后续，不必再调研）

### 4.1 CaPilot 没有节点画布

现有「canvas」= xterm HTML canvas、批注截图、CommitGraph 量字、图片视口。  
主内容是 tab：`agent | editor | diff | image`。  
对照 [cleancode](https://github.com/chen-985211/cleancode)：那才是 canvas-first（BlockGraph + Run + Agent 分事实源）。CaPilot 若要加画布，应做 **worktree 级视图**，PTY 后挂到节点，不要把 ContentArea 整页换成 React Flow。

### 4.2 真正拖慢 runtime 新增的

不是 `AgentRuntimeAdapter` trait，而是做成「一等 Agent」时的横切：

1. `Composer.tsx` 里 per-runtime 按键剧本（最大）
2. hook / JSONL 推断 / 活动启发式 三套状态
3. resume + `AgentUsage` 会计
4. 全仓 hard-code 名单（模板 union、mouseProtocol、usage allow-list、slash）
5. 未知 id 以前默认 Claude

### 4.3 OpenCode 类全屏 TUI 在 PTY 里

模型冲突：TUI 当 framebuffer 每帧重画，xterm 当格子终端。不能靠 `--no-alt-screen`（那是 Codex 的活）。已有宿主策略：resident 面板、回前台 `fit+refresh`、锁 `min-height:0`、不透明黑 viewport、自管 SGR 滚轮。  
这些是 **v2 / 已有 runtime** 的债，新 runtime v1 不要抄。画布节点里嵌这类 TUI 会稳定失败。

---

## 5. 建议下一步（本分支）

1. v1 表已接：`generic.rs` 的 `V1_RUNTIMES`（Orca harness + CodeBuddy/Qoder）。未装的 CLI 在 Settings 显示「未检测」，不阻塞。
2. Settings 版本号已收短（`5.3.9`）。收起面板 + 每行启用开关 **只记在 skill**，v1 未实现。
3. 不要把主仓其它 dirty 文件 cherry-pick 进来。
4. v2：Composer capability 表；把 `PLAIN_COMPOSER` 翻回 `false` 即可恢复按键栏与 `/` 目录。Settings 启用开关见 skill「Settings UX deferred」。

---

## 6. 不要做的

- 把未知 runtime 再改回 Claude fallback
- 为 v1 改 Composer / mouseProtocol / status_hooks
- 在 canvas / React Flow 里嵌 OpenCode
- 把 `dsh.rs` / `opencode.rs` 当模板复制
