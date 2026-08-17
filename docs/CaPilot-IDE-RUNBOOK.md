# CaPilot IDE — 运行与维护手册

> **日期:** 2026-08-10
> **定位:** 项目的「如何跑 / 已知坑 / 文档地图」运行手册。
> 安全细节见 [security-review.md](security-review.md)。

---

## 1. 运行 / 构建

前置要求：Rust 1.97+、Node.js 24+、pnpm、claude CLI（`pnpm tauri dev` 需要）。Linux 额外一次性系统依赖：

```bash
sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
```

常用命令：

| 命令 | 位置 | 说明 |
| --- | --- | --- |
| `pnpm install` | 仓库根目录 | 安装前端依赖 |
| `pnpm tauri dev` | 仓库根目录 | 开发模式（需 claude CLI；Linux 系统依赖见上）|
| `pnpm tauri build` | 仓库根目录 | 打包发布 |
| `cargo test` | `src-tauri/` | Rust 单元测试（24 个）|
| `pnpm tsc --noEmit` | 仓库根目录 | TS 类型检查 |

> 根目录 `README.md` 也含一份快速上手（Quick Start + 前置条件），可作为入口。

## 2. 设计规范（LUCY）与资源

IDE 遵循 CaPilot 主仓库的 **LUCY styleguide**（8-bit Pixel × Apple Smooth，深色科技 + 紫色强调）。完整规范见 `docs/styleguide/ui-style-guide.md`（附 `logo-preview.html` / `preview.png`）。

要点速查：

- **色彩**：`--bg #07090F`、`--brand #8B5CF6`（紫），状态色仅绿/黄/红
- **字体**：FusionPixel-Prop（像素标签 / 标题）/ Tektur（正文）/ JetBrainsMono（技术）
- **边框**：2px 实线 + 硬阴影 `4px 4px 0`，几乎无圆角
- **动效**：Apple 曲线 `cubic-bezier(0.25, 0.1, 0.25, 1)`

**运行时资源位置：**

- 字体内嵌于 `public/fonts/`：`JetBrainsMono-{Regular,Bold}.ttf`、`FusionPixel-Prop-zh_hans.ttf`（12px 比例）、`Tektur-{Regular,Medium}.ttf`
- logo 在 `public/logo.png`（源文件见 `ui/assets/logo/`，UI 经 `public/` 引用）
- 颜色令牌定义在 `ui/App.css :root`（`@font-face` 引用 `/fonts/*.ttf`，全本地、无 Google Fonts）
- 应用图标统一放在 `ui/assets/app-icon/`（由 `tauri.conf.json` `bundle.icon` 以 `../ui/assets/app-icon/*` 引用）。源图为 quantum 风格 `icon.png`（512）。Linux/deb 使用常见 hicolor 尺寸 `16/24/32/48/64/128/256/512` PNG（桌面项 `Icon=capilot-ide`）；Windows 用 `icon.ico`。换源图后从 `icon.png` 重导出各尺寸（`pnpm tauri icon` 可重生 ico），再重建 deb。**发布平台仅 Linux + Windows**（CI / bundle targets 不含 macOS）。

**同步规则：** `docs/styleguide/` 与 `ui/assets/`（原 `docs/Assets/`）是主仓库 `Doc/styleguide/`、`Doc/Assets/` 的复制品，**改设计需两边同步**。

## 3. 已知问题与技术债

### 已知技术债（Medium/Low，均未修）

- `.lock().unwrap()` 毒化处理（多处 std Mutex）
- `git_status` 未跟踪大文件整读入内存（应流式）
- `Persistence::open` 启动 expect（`$HOME` 不可写会 panic）

> 已解决（2026-08-06）：「会话 permissionMode 未持久化」已在会话生命周期改造中一并完成 —— mode/speed/model 持久化进 `sessions` 表，Composer 三设置跟随当前会话。

> 已解决（2026-08-15）：OpenCode 常驻 PTY 重挂载后滚轮失效 / 任意区域切 prompt 历史。修复约束与协议记录见 `ai-runtime-references.md` 的 “TUI 鼠标协议”，回归命令为 `pnpm test:terminal-mouse`。

> 已决定（2026-08-17）：**取消 macOS 官方支持与发布**。Release CI 只构建 Linux/Windows；bundle targets 为 `deb` / `appimage` / `nsis`；updater `latest.json` 不再包含 `darwin-*`。源码里少量 `#[cfg(target_os = "macos")]` 路径保留无害，但无预编译包、无签名流程。

> 已落地（2026-08-17）：**Windows agent CLI 解析**（参考 Paseo）。`agent_runtime/executable.rs` 对 bare name 做 `PATH`+`PATHEXT` 解析，`.cmd`/`.bat`（npm 全局 shim）经 `cmd.exe /d /s /c` 包装后再进探测与 PTY（ConPTY 不应用 PATHEXT、也不能直接 CreateProcess 脚本）。`cli_available` / `cli_version` / 各 runtime 的非 PTY `Command` 与 `pty_core::spawn` 共用此路径。bash/Git Bash 仍是可选用户 shell，不再是 agent 启动前提。

> 已落地（2026-08-17）：**默认交互终端 = OS shell**。新 runtime `shell`：Windows 优先 `pwsh` 否则 `ComSpec`/cmd；Unix 用 `$SHELL`/bash/sh。新建终端模板固定第一项为「终端」；`bash-rc`（Git Bash）降为可选（未安装则隐藏）。文件树「在此打开终端」与快速启动走 `shell`。Agent 启动仍不经过 shell。

### 待开发项

- **编辑器外部改动监视**（notify → 前端刷新）：Git 面板已用 2.5s 前端轮询兜底，编辑器标签页本身仍未监听磁盘改动

## 4. 安全注意事项

> 完整细节见 `docs/security-review.md`（CSP / capabilities / 路径白名单 / IPC 暴露逐条 + 发布前 checklist）。

- **信任边界**：`agent_write` 是高权限命令，信任边界是「打包的前端受信任」——单窗口设计下 XSS 即完全控制应用，靠纵深防御缓解（严格 CSP、无远程内容）。
- **范围收紧（发布前）**：`fs_*` / `git_*` 范围限制建议发布前收紧（git 命令接受任意 `repo` 路径、`fs_write` 可写 `$HOME` 任意处，含 dotfile）；`fs_write` 存在 symlink 逃逸的 fallback bug（见 security-review §3）。
- **updater 占位**：updater 配置是占位 endpoint/pubkey；空 `pubkey` 会跳过签名校验，**发布前必须填真实 HTTPS endpoint 与签名公钥**。

## 5. 文档索引

| 文档 | 位置 | 内容 |
| --- | --- | --- |
| 本手册 | `docs/CaPilot-IDE-RUNBOOK.md` | 运行 / 已知坑 / 文档地图 |
| 安全审查 | `docs/security-review.md` | CSP / 权限 / 路径 / IPC 审查与发布前 checklist |
| LUCY 风格 | `docs/styleguide/` | 设计规范（源：主仓库 `Doc/styleguide/`）|
