# dsh 适配迁移 + Composer 三 bug 修复 — 交接报告

> 分支：`respond-to-greeting`（未合并到其他分支）。日期：2026-08-15。
> 本次同时完成：① dsh-TUI 升级适配（`dsh-cc-tui` / profile `cc-tui` → `@deepseek-harness-tui/dsh-tui` 0.6.1 / profile `dsh-tui`）；② 三 个 Composer 链接终端 bug（思考强度、权限、模型列表）在新包上的修复。当前事实以 `docs/ai-runtime-references.md` §2.4 / §3 为准。

## 1. 做了什么

### dsh-tui 0.6.1 迁移（`src-tauri/src/agent_runtime/runtimes/dsh.rs` 为主）

- profile 全量切换：`cc-tui` → `dsh-tui`（`dsh_tui_profile_dir`、patch id `- id: dsh-tui`、`spawn_interactive`/`status_hook_args` 的 `--profile dsh-tui`、preflight 的 `--dump-config --profile dsh-tui`、诊断文案）。
- **模型不再是 hard-code**：新增 `model_catalog_probe` 读 `~/.dsh/settings.yaml`（复用 dsh 自己的 js-yaml），返回 provider-qualified 列表；`agent-default-model` 标默认；deepseek-official 路由内建 flash/pro。默认模型 = settings.yaml 的 `agent-default-model`（本机实测 `opencode-go/deepseek-v4-flash`）。
- **思考档位**：deepseek-official 路由继续按 speed 钉 `effort: off|high|max`；**pi-ai 路由（opencode-go 等）固定 `effort: off`**——pi-ai 对未声明 reasoning 元数据的模型只支持 off，钉 off 覆盖机器残留 `~/.dsh-cc/effort.json=high`，状态行显示真实档位、不会请求期抛 `UNSUPPORTED_REASONING_EFFORT`。
- **权限**：dsh-tui 挂载 `dsh-permission-presets`，`/permission read-only|workspace-write|danger-full-access` 实时切换并落盘**持久** session-log 事件（`permission/preset`+`sandbox/mode`+`approval/policy`）。Composer 的 dsh 权限分支从「kill+重启」改为**驱动 TUI 命令**。
- 数据目录 / 环境变量**未变**（实测 0.6.1 仍用旧名，官方文档声称 DSH_TUI_* 不实）：`~/.dsh-cc/`（effort.json / resume.txt）、`DSH_CC_RESUME_SESSION` / `DSH_PERMISSION_MODE` / `CC_TUI_*`。`dsh_default_effort`、`capture_resume_key`、`read_resume_txt` 无需改动。

### Composer 三 bug 修复（`ui/components/layout/Composer.tsx`）

1. **思考强度对应不上**：auto 位置 2→1（dsh 默认档是 High，不是 Max）；选项标签 Off/High/Max 与 dsh 词汇对齐；Shift+Tab 环距驱动**仅限 deepseek-official 路由**（pi-ai 会话只持久化 speed、不驱动 TUI）；⚡ 菜单对 pi-ai 只留 auto/off。
2. **权限切换不了**：走 `/permission <preset>` live 切换（见上），不再 kill+重启。
3. **模型列表不全**：由 settings.yaml 探测补齐（opencode-go + deepseek-official 内建），provider-qualified 显示，默认标记正确。

### 其他

- 预检探针（`preflight_diagnostic`）与 fast-exit 安全网（`agent://exit-diagnostic`，lib.rs）随 profile 名更新到 dsh-tui；preflight 的 node 解析补了 ESM-subpath 回退（修 `@deepseek-harness-tui/dsh-tui/working-activity` 误报）。
- `SettingsModal.tsx` 启动编辑器静态前缀、`slash.rs` 注释、`lib.rs` 诊断文案同步 dsh-tui。
- 文档：`docs/ai-runtime-references.md` §2.4 + §3 表（行 19/20/21）全量迁移；`docs/dsh-runtime-integration.md` 顶部加迁移通告（注明权限 live 切换、动态模型目录、旧 env 名仍在）。

## 2. 验证了什么（命令 + 结果）

| 验证 | 命令 | 结果 |
| --- | --- | --- |
| Rust 全量测试 | `cd src-tauri && cargo test` | ✅ lib **165 passed / 0 failed / 1 ignored**（25 dsh + 8 slash）+ 集成冒烟 **3 passed** |
| 前端类型 | `cd ui && pnpm tsc --noEmit` | ✅ exit 0 |
| 预检探针全绿 | 逐个 require.resolve `dsh --dump-config --profile dsh-tui` 的 82 个 `name:` | ✅ 全部可解析（含 ESM-subpath 回退后的 `working-activity`） |
| 模型目录探针 | 复刻 `model_catalog_probe` 的 node 脚本 | ✅ `{"pi":[{"provider":"opencode-go","models":[{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"}]}],"deepseek":[],"default":{"provider":"opencode-go","model":"deepseek-v4-flash"}}` |
| 默认路由 patch 组合 | `dsh --dump-config --profile dsh-tui --patch og-patch.yml` | ✅ 组合出行含 `provider: opencode-go / model: deepseek-v4-flash / effort: 'off' / sessionId` |
| 两条路由真实启动 | `pty.fork` 下 `dsh --profile dsh-tui --patch …`（deepseek-official + opencode-go 各一次，NODE_ENV=production，11s 后 kill） | ✅ 均存活启动；session log 首部 = `session` 头 + `permission/preset{workspace-write}` + `sandbox/mode{workspace-write}` + `approval/policy{ask}` + `activity/status` |
| 会话身份 / resume | 抽样 9 个 `~/.dsh/sessions/**/` 子目录 | ✅ **子目录名 == 头 `id`**（uuid 与 `session-<uuid>` 两种命名都匹配），头含 `cwd` → `detect_recent_resume_key` / `DSH_CC_RESUME_SESSION` 链路成立 |
| 状态推断事件类型 | grep 新包 `@deepseek-ai/dsh-session/lib/index.js` | ✅ `"turn/start"`×4、`"turn/end"`×4、`"assistant/chunk"`×3、`"request/header"`×5、`"permission/preset"`、`"sandbox/mode"`、`"approval/policy"` 逐字存在 → `infer_status_from_content` / usage 解析无需改 |
| 权限 preset 对映射 | 读 `dsh.rs::list_permission_modes` + Composer dsh 分支 | ✅ `ask→read-only`、`auto→workspace-write`、`yolo→danger-full-access` 与 dsh-permission-presets 一致 |

## 3. 哪些没验证及原因

1. **真实 TUI 交互（按键驱动）**：PTY 测试只证明了「能启动 + 写持久事件」，没有真正向 TUI 发 Shift+Tab / `/permission`。本机 Wayland 无输入注入，无法驱动 TUI 内按键。`/permission` 的持久性靠「env→启动时落盘同样事件」这一等价机制 + dump-config preset 行验证，非按键实测。
2. **完整请求生命周期**：只在启动段观测到 `session`/`permission/preset`/`sandbox/mode`/`approval/policy`/`activity/status`；未发真实消息，故未见 `turn/start`→`request/header`→`assistant/chunk`→`turn/end` 实样。事件类型已在新包源码确认，但端到端序列未跑。
3. **CaPilot GUI 端到端**（新建 tab → 模型列表 → 权限环 → 思考菜单 → resume）：Wayland 无法做 UI 自动化，只能代码走读 + CLI 验证。
4. **resume 全链路**（spawn → /exit → 恢复会话）未在 CaPilot 里走通一遍。
5. **三平台**：只验证 Linux；dsh.rs 无平台专属代码，但 Windows 权限映射（无沙箱后端）沿用设计文档 §4.4 的既有说明，未实测。

## 4. 我醒来后该手动测什么

1. **新建 dsh 终端**（默认模型应为 `opencode-go/deepseek-v4-flash`）：应正常打开，状态行显示 **Off effort**（pi-ai 固定 off，而非高）。
2. **模型列表**：打开模型选择器，应看到 `opencode-go/deepseek-v4-flash`（默认）、`deepseek-official/deepseek-v4-flash`、`deepseek-official/deepseek-v4-pro`。若为空/不全 → 检查 `~/.dsh/settings.yaml`（CaPilot 从它读 provider 模型）。
3. **思考强度**：在 **opencode-go 会话**里点 ⚡ 应只有 Auto / Off（id 为 `auto`/`fast`）；切到 **deepseek-official** 会话应看到 Auto / Off / High / Max，且 High↔Max 切换后 TUI 状态行实时变化。
4. **权限**：在**运行中**的 dsh 会话切换 ask↔auto↔yolo，TUI 应立即生效；`/exit` 后 resume 该会话，权限应保持（持久事件覆盖 env）。
5. **回归**：dsh 建终端后若又出现「闪一下即关」（exit 0 无 stderr），应弹「终端启动失败: dsh-tui 配置中有插件包无法加载…」，按提示核对 profile；正常长会话**不应**触发该通知。
6. **旧 profile 清理（可选）**：CaPilot 现在只认 `~/.dsh/profiles/dsh-tui`；`~/.dsh/profiles/cc-tui` 确认无用后可 `dsh plugin --profile cc-tui remove dsh-cc-tui`（不影响新 profile）。
