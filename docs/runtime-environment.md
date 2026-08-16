# Runtime 环境要求与安装指南

> 面向使用 CaPilot 的用户 / 维护者：本文说明在 CaPilot 里使用 bash / claude / opencode / codex / dsh 这五类 runtime 需要怎样的系统环境、缺失时如何安装，以及 CaPilot 具体是怎么检测它们的（`is_available` / `is_authenticated` 的实现）。

---

## 0. 总览

CaPilot 不维护自己的 runtime，而是**检测本机 PATH 上已有的 CLI**，并在会话中把它作为真实终端（PTY）跑起来。因此：

- 每个 CLI 的安装、登录、升级都发生在 **CaPilot 之外**（用各自的官方安装方式）。
- CaPilot 的「已安装」列表 = 检测通过的结果，不是 CaPilot 装好的东西。
- 检测是**只读探测**：`runtime_list_available` 命令（`src-tauri/src/lib.rs:1751`）遍历 `known_runtimes()`（`src-tauri/src/agent_runtime/runtimes/mod.rs:26`，即 `claude` / `codex` / `opencode` / `dsh` / `bash-rc`），对每个 adapter 调用 `is_available()` 和 `is_authenticated()`。它**不改用户任何全局配置**，也不会在检测时登录或初始化什么。

| Runtime | 可用性检测（`is_available`） | 认证检测（`is_authenticated`） | 模型来源 |
|---|---|---|---|
| bash | `bash --version` 成功 | 恒为 `true` | 无 |
| claude | `claude --version` 成功 | 恒为 `false`（故意不探测） | 硬编码 3 个模型 |
| codex | `codex --version` 成功 | `codex login status` 成功 | `codex app-server`（stdio JSON-RPC） |
| opencode | `opencode --version` 成功 | `opencode models --verbose` 能解析出非空模型 | 同上（`opencode models` 输出） |
| dsh | `dsh --version` 成功 **且** `~/.dsh/profiles/dsh-tui` 存在 | `DEEPSEEK_API_KEY` 环境变量 或 `~/.dsh/.credentials.yaml` 存在 | `~/.dsh/settings.yaml` 目录探测 + 内建 deepseek-official |

前端「已安装」列表与「已登录 / 已安装」徽标就来自这个命令（`ui/state/runtime.ts:11`、`ui/components/layout/SettingsModal.tsx:54`）。另外，**启动会话时会再次把关**：spawn 前 `if !adapter.is_available()` 直接拒绝（`lib.rs:234`；resume 同，`lib.rs:907`）。

---

## 1. 通用前置

无论用哪个 runtime，都需要：

- **PATH 里能找到对应的 CLI**（安装完成后开新终端验证 `command -v <cli>`）。
- **Node.js 18+**：claude / codex / opencode（npm 版）/ dsh 都是 Node 系工具。若你用的是 standalone 二进制版（如 opencode 安装脚本、codex Homebrew/cargo 版），Node 不是必需的，但装 CLI 时大多仍以 npm 分发。
- 各 provider 的**有效凭据**（账号登录或 API Key），否则 CLI 能 `--version`（检测通过）但启动后立刻报未登录。

---

## 2. Bash

### 2.1 系统环境要求

- Linux / macOS：几乎总是自带 `/bin/bash`（≥ 4.x），无需安装。
- Windows：CaPilot 是 Tauri 桌面应用，跑 bash 需要 WSL 或 Git Bash 等把 `bash` 放进 PATH。

### 2.2 安装（缺失时）

```bash
# Debian / Ubuntu
sudo apt install bash

# macOS（brew 装的 bash 在 /opt/homebrew/bin，需确保在 PATH 前面）
brew install bash
```

### 2.3 CaPilot 如何检测

`src-tauri/src/agent_runtime/runtimes/bash.rs:20-26`：

```rust
fn check_available() -> bool {
    Command::new("bash").arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}
```

- `is_available()` = `bash --version` 退出码为 0（bash.rs:42-44）。
- `is_authenticated()` 恒为 `true`（bash 无登录概念，bash.rs:46-48）。
- 无模型列表、无权限档位。两个 id：`bash`（`--norc` 最小化）与 `bash-rc`（完整交互式，sources 用户的 `~/.bashrc`）；「已安装」列表只提供 `bash-rc`（`runtimes/mod.rs:22-26`）。

---

## 3. Claude Code（runtime `claude`）

### 3.1 系统环境要求

- Node.js 18+（Claude Code 运行在 Node 上）。
- 认证：Claude 订阅账号（`claude` 交互式登录）或 `ANTHROPIC_API_KEY`。

### 3.2 安装（缺失时）

```bash
# npm 全局安装（官方推荐）
npm install -g @anthropic-ai/claude-code

# 或官方安装脚本
curl -fsSL https://claude.ai/install.sh | bash
```

安装后首次使用需登录：在终端跑 `claude`，按提示完成 OAuth 登录（或设置 `ANTHROPIC_API_KEY`）。验证：`claude --version`。

### 3.3 CaPilot 如何检测

`src-tauri/src/agent_runtime/runtimes/claude.rs:41-47`：

```rust
fn check_available() -> bool {
    Command::new("claude").arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}
```

- `is_available()` = `claude --version` 退出码为 0。
- `is_authenticated()` **恒为 `false`**（claude.rs:219-225）：故意不探测登录状态——凭据文件检查曾把已过期的会话误报为「已登录」，而可用性已经被 `is_available()` 把关，所以只在 UI 上体现「已安装」。
- 模型列表**硬编码**（claude.rs:227-252）：`claude-sonnet-5`（默认）/ `claude-opus-5` / `claude-haiku-4-5`，不查询 CLI。

---

## 4. Codex（runtime `codex`）

### 4.1 系统环境要求

- Node.js 18+（npm 版）；或用 Homebrew / cargo 版（cargo 需 Rust toolchain）。
- 认证：ChatGPT 账号（`codex login` 走 ChatGPT 授权）或 `OPENAI_API_KEY`。

### 4.2 安装（缺失时）

```bash
# npm 全局安装
npm install -g @openai/codex

# 或 Homebrew
brew install openai/codex/codex

# 或从源码（Rust）
cargo install codex
```

登录：`codex login`（浏览器授权 ChatGPT 账号），或设置 `OPENAI_API_KEY`。验证：`codex --version` 和 `codex login status`。

### 4.3 CaPilot 如何检测

`src-tauri/src/agent_runtime/runtimes/codex.rs:82-96`：

```rust
fn check_available() -> bool {
    Command::new("codex").arg("--version").output()
        .map(|output| output.status.success()).unwrap_or(false)
}

fn check_authenticated() -> bool {
    Command::new("codex").args(["login", "status"]).output()
        .map(|output| output.status.success()).unwrap_or(false)
}
```

- `is_available()` = `codex --version` 退出码为 0。
- `is_authenticated()` = `codex login status` 退出码为 0（这个子命令会返回当前登录态）。
- 模型列表不是硬编码：拉起 `codex app-server --listen stdio://`，通过 stdio JSON-RPC（`initialize` + `model/list`）读**已安装 CLI 的真实模型目录**（codex.rs:101+），随认证 / 账号 / hidden-model 策略变化。

---

## 5. OpenCode（runtime `opencode`）

### 5.1 系统环境要求

- Node.js 18+（npm 版）；standalone 二进制版无需 Node。
- 认证：在 opencode 配置中给各 provider 配 API Key（如 `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`OPENROUTER_API_KEY` 等），无统一「登录」概念。

### 5.2 安装（缺失时）

```bash
# 官方安装脚本（standalone 二进制）
curl -fsSL https://opencode.ai/install | bash

# 或 npm
npm i -g opencode-ai

# 或 Homebrew
brew install sst/tap/opencode

# 或从源码
cargo install opencode-rs
```

验证：`opencode --version`，以及 `opencode models` 能列出模型（需要有 provider 凭据）。

### 5.3 CaPilot 如何检测

`src-tauri/src/agent_runtime/runtimes/opencode.rs:191-197` 与 `372-377`：

```rust
fn check_available() -> bool {
    Command::new("opencode").arg("--version").output()
        .map(|output| output.status.success()).unwrap_or(false)
}

fn check_authenticated() -> bool {
    // 比查 auth.json 更强的就绪测试：catalog 含本安装可用的 provider
    !Self::discover_models().is_empty()
}
```

- `is_available()` = `opencode --version` 退出码为 0。
- `is_authenticated()` = `discover_models()` 非空：执行 `opencode models --verbose`（旧版回退 `opencode models`，opencode.rs:338-369）并解析出非空模型列表。因为 opencode 的 catalog 即使没配凭据也会列出「credential-free」provider，所以这比 `auth.json` 存在与否更能代表就绪状态。
- 模型列表来源同上（`opencode models --verbose` 的 provider/model 输出），带 OpenCode TUI 原生显示名。

---

## 6. dsh（runtime `dsh`，DeepSeek Harness）

### 6.1 系统环境要求

- Node.js（CLI 是 npm 包）+ **pnpm**（`dsh plugin` 会把剩余参数转发给 profile 目录里的 pnpm）。
- 认证：`DEEPSEEK_API_KEY` 环境变量，或 `~/.dsh/.credentials.yaml`；`llm-pi-ai` 提供商标在 `~/.dsh/settings.yaml` 里各自声明密钥 env。

### 6.2 安装（缺失时）

```bash
# 1) 装 CLI
npm install -g @deepseek-ai/dsh

# 2) 装 pnpm（corepack 或 npm）
corepack enable          # 或 npm i -g pnpm

# 3) 创建 dsh-tui profile（CaPilot 可用性的硬性前提，必须先做）
dsh plugin --profile dsh-tui add @deepseek-harness-tui/dsh-tui

# 4) 认证
export DEEPSEEK_API_KEY=sk-...        # 或写 ~/.dsh/.credentials.yaml
```

验证：`dsh --version`、`dsh --dump-config --profile dsh-tui`（核对组合配置）、`ls ~/.dsh/profiles/dsh-tui`。

### 6.3 CaPilot 如何检测

`src-tauri/src/agent_runtime/runtimes/dsh.rs:872-881`：

```rust
fn is_available(&self) -> bool {
    // 光有 dsh 不够：TUI 从 dsh-tui profile 启动，profile 必须已创建一次
    Self::check_available() && Self::dsh_tui_profile_dir().is_some_and(|dir| dir.exists())
}
fn is_authenticated(&self) -> bool {
    std::env::var_os("DEEPSEEK_API_KEY").is_some()
        || Self::dsh_home().is_some_and(|home| home.join(".credentials.yaml").exists())
}
```

- `is_available()` = `dsh --version` 退出码为 0（dsh.rs:552-558）**且** `~/.dsh/profiles/dsh-tui` 存在（`$DSH_HOME` 覆盖默认 `~/.dsh`，dsh.rs:87-105）。与其它 runtime 不同，光有 CLI 不够——profile 是 dsh-TUI 首次启动的前置。
- `is_authenticated()` = `DEEPSEEK_API_KEY` 环境变量存在 **或** `~/.dsh/.credentials.yaml` 存在（dsh.rs:878-881）。
- 模型列表：读 `~/.dsh/settings.yaml` 的 `llm-pi-ai.providers` + `llm-deepseek.models`，合并内建 `deepseek-official`（flash / pro），id 为 `provider/model` 限定（dsh.rs:886-907）。
- 额外有 `preflight`：`dsh --dump-config --profile dsh-tui` 探测插件能否 `require.resolve`（dsh.rs:568-579），插件缺失时在 UI 给出中文诊断，但**不阻塞 spawn**（探测本身不可靠时放行，由退出兜底网接住）。

---

## 7. CaPilot 检测机制细节

1. 前端请求 `runtime_list_available` → Rust 遍历 `known_runtimes()`（`runtimes/mod.rs:26`）→ 每项构建 `RuntimeInfo { id, name, available, authenticated, models, permission_modes, thinking_options }`（`lib.rs:1751-1767`）。
2. 每个 adapter 实现 `AgentRuntimeAdapter`（`agent_runtime/adapter.rs:157`）：`is_available()` / `is_authenticated()` 的语义与实现见上文各节。
3. 检测全程**子进程 + 退出码**判断，不解析凭据文件内容（唯一例外：dsh 查 `~/.dsh/.credentials.yaml` 是否存在、opencode 用模型列表作就绪度）。
4. **二次把关**：spawn / resume 前再调一次 `is_available()`，失败即拒绝并提示（`lib.rs:234`、`lib.rs:907`），避免「列表显示可用但进程起不来」。
5. 检测与**状态钩子注入无关**：`is_available` 只关心 CLI 装没装；生命周期状态上报靠启动时的 per-session 注入（claude `--settings`、codex `-p` profile、opencode 插件、dsh `--patch`），装好后自动生效。

---

## 8. 常见问题

| 现象 | 原因 / 排查 |
|---|---|
| CLI 已安装但「已安装」列表不出现 | PATH 没生效（装完要开新终端）；或该 CLI 的 `--version` 在该环境退出码非 0。先手动跑 `command -v <cli>` 与 `<cli> --version` |
| claude 显示「已安装」但启动报未登录 | CaPilot 故意不探测 claude 登录态（claude.rs:219-225）；请先在系统终端跑 `claude` 完成登录 |
| codex 显示「未登录」 | `codex login status` 非 0。执行 `codex login` 或配置 `OPENAI_API_KEY` |
| opencode 显示「未登录」 | `opencode models --verbose` 解析为空。确认至少一个 provider 已配 API Key |
| dsh 显示「未安装」 | 大概率是缺 `~/.dsh/profiles/dsh-tui`：执行 `dsh plugin --profile dsh-tui add @deepseek-harness-tui/dsh-tui`（dsh 的 profile 是硬性前提） |
| dsh 显示「已安装」但启动即退出 | 跑 `dsh --dump-config --profile dsh-tui` 看插件解析诊断；CaPilot 的 preflight 也会在设置页给出原因（dsh.rs:568-579） |
