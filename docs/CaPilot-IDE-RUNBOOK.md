# CaPilot IDE — 运行与维护手册

> **日期:** 2026-08-18
> **定位:** 项目的「如何跑 / 已知坑 / 文档地图」运行手册。
> 安全细节见 [security-review.md](security-review.md)。

---

## 1. 运行 / 构建

前置要求：Rust 1.97+、Node.js 24+、pnpm、claude CLI（`pnpm tauri dev` 需要）。

**Linux 构建依赖（-dev，一次性）：**

```bash
sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
```

**Linux 运行时媒体（视频壁纸）：** WebKitGTK 的 `<video>` 走 GStreamer。`.deb` 的
`Depends` 已声明 `gstreamer1.0-libav` + `gstreamer1.0-plugins-bad`（需启用
Ubuntu universe）。AppImage / 源码运行不会自带解码插件，主机需自行安装：

```bash
# Debian / Ubuntu
sudo apt install gstreamer1.0-libav gstreamer1.0-plugins-bad

# Fedora
sudo dnf install gstreamer1-libav gstreamer1-plugins-bad-free

# Arch
sudo pacman -S gst-libav gst-plugins-bad
```

装完可用 `gst-inspect-1.0 avdec_h264 | head` 确认 H.264 解码器存在。Windows
走 WebView2 系统解码，无需额外包。内置主题视频统一为 H.264（`to-720p.sh`）。

**仅 Linux 打包**的 `<video>` 走 WebKitGTK + GStreamer。GStreamer 只认它能
自己打开的 URI（`file://`、真正的 `http(s)://`），**不走** WebKit 自定义协议
回调，所以 `asset://` / `tauri://` / `blob:` 都会零帧。安装版因此在进程内起一个
`127.0.0.1:<随机端口>` HTTP 服务（`wallpaper_http.rs`，始终 `Accept-Ranges`，
Range 不截断），`<video src>` 指向 `http://127.0.0.1:<port>/wallpaper/<path>`。
CSP `media-src` 必须写 `http://127.0.0.1:*`（无端口只匹配 :80，随机端口会被拦）。
`pnpm tauri dev` 仍走 Vite HTTP；Windows WebView2 / macOS WKWebView 仍走
`asset://`。静态图一律 `asset://`。

Agent 可重复验证（不依赖截屏；本机 Wayland 截不到原生 GTK 窗口）：

```bash
# 1) 文件本身能解（GStreamer）
gst-discoverer-1.0 file:///usr/lib/CaPilot/themes/wallpapers/capilot.mp4
timeout 3 gst-play-1.0 --no-interactive --audiosink=fakesink --videosink=fakesink \
  /usr/lib/CaPilot/themes/wallpapers/capilot.mp4

# 2) 同一 WebKitGTK 上哪种 URL 能出帧（videoWidth>0）
python3 scripts/webkit-video-probe.py --mode all --timeout 10
# 2026-08-21 实测：file + http 出帧；blob + capilot-media 零帧（MEDIA_ERR_SRC_NOT_SUPPORTED）
```

常用命令：

| 命令 | 位置 | 说明 |
| --- | --- | --- |
| `pnpm install` | 仓库根目录 | 安装前端依赖 |
| `pnpm tauri dev` | 仓库根目录 | 开发模式（需 claude CLI；Linux 系统依赖见上）|
| `pnpm tauri build` | 仓库根目录 | 打包发布 |
| `./reinstall-deb.sh` | 仓库根目录 | 卸掉本机 `ca-pilot`、只打 `.deb`、再装上（用户数据不动）|
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

### 已知技术债

> 已解决（2026-08-18）：**`.lock().unwrap()` 毒化恢复**。
> - `SessionStore::lock_db` / `Persistence::lock_db`：poisoned mutex 经 `into_inner()` 恢复，避免一次 panic 后所有 DB 命令连环炸。
> - `db_tolerant()` 改为走 `lock_db()`（毒化时仍可用）。
> - `lib.rs` 中 sessions DB 的 `db().lock().unwrap()` / `if let Ok(db) = db().lock()` 统一为 `lock_db()`。
> - 其它热路径 Mutex（`resource` / `output_hub` / `fs_search` / `git_gate` / `pty_core` / `cat_breeds` / status cache）同样 `unwrap_or_else(|p| p.into_inner())`。

> 已解决（2026-08-18）：**`git_status` 未跟踪大文件整读入内存**。
> - 未跟踪文件的 `+N` 行数已用 `count_lines` 流式按行计数（`BufRead`，上限 1M 行），不再 `read_to_string` 整文件（`lib.rs` `git_status` / `count_lines`）。

> 已解决（2026-08-18）：**`Persistence::open` 启动 expect**。
> - `run()` 不再 `Persistence::open().expect(...)`；失败时 `eprintln` + `log::error` 后 `std::process::exit(1)`，避免 `$HOME` / data root 不可写时不透明 panic。
> - 数据根不可写安装位置的回退见同日「数据根 + 多盘路径」条目（`ensure_data_root` / `install_dir_is_user_writable`）。

> 已解决（2026-08-06）：「会话 permissionMode 未持久化」已在会话生命周期改造中一并完成 —— mode/speed/model 持久化进 `sessions` 表，Composer 三设置跟随当前会话。

> 已解决（2026-08-15）：OpenCode 常驻 PTY 重挂载后滚轮失效 / 任意区域切 prompt 历史。修复约束与协议记录见 `ai-runtime-references.md` 的 “TUI 鼠标协议”，回归命令为 `pnpm test:terminal-mouse`。

> 已决定（2026-08-17）：**取消 macOS 官方支持与发布**。Release CI 只构建 Linux/Windows；bundle targets 为 `deb` / `appimage` / `nsis`；updater `latest.json` 不再包含 `darwin-*`。源码里少量 `#[cfg(target_os = "macos")]` 路径保留无害，但无预编译包、无签名流程。

> 已落地（2026-08-17）：**Windows agent CLI 解析**（参考 Paseo）。`agent_runtime/executable.rs` 对 bare name 做 `PATH`+`PATHEXT` 解析，`.cmd`/`.bat`（npm 全局 shim）经 `cmd.exe /d /s /c` 包装后再进探测与 PTY（ConPTY 不应用 PATHEXT、也不能直接 CreateProcess 脚本）。`cli_available` / `cli_version` / 各 runtime 的非 PTY `Command` 与 `pty_core::spawn` 共用此路径。bash/Git Bash 仍是可选用户 shell，不再是 agent 启动前提。

> 已落地（2026-08-17）：**默认交互终端 = OS shell**。新 runtime `shell`：Windows 优先 `pwsh` 否则 `ComSpec`/cmd；Unix 用 `$SHELL`/bash/sh。新建终端模板固定第一项为「终端」；`bash-rc`（Git Bash）降为可选（未安装则隐藏）。文件树「在此打开终端」与快速启动走 `shell`。Agent 启动仍不经过 shell。

> 已落地（2026-08-17）：**Windows 进程树回收 + 文件树/快速启动按 shell 语义适配**。`pty_core::kill` / 探测超时走 `process_kill::kill_process_tree`（Windows `taskkill /T /F`，Unix 进程组 SIGKILL），避免 agent 孙进程残留。文件树「运行此文件 / 在当前终端打开」按当前 shell 风味（PowerShell / cmd / POSIX）生成 `cd` 与引号；`.py` 在 Windows 用 `python`，并识别 `.ps1`/`.bat`/`.cmd`。快速启动编辑框提示命令注入的是系统默认 shell（非 bash）。

> 已落地（2026-08-18）：**数据根 + 多盘路径**。
> - 可写安装目录（便携版 / 当前用户 NSIS / 用户自有 `/opt/...`）：数据目录 = `<安装目录>/data`。首次启动若 `data/` 为空且存在旧版 `~/CaPilot/sessions.db`，会一次性迁移过去。
> - 不可写安装位置（`deb` → `/usr/bin`、AppImage 只读挂载、Program Files 全用户安装）与开发态（`target/debug|release`）：数据目录 = `~/CaPilot`。**不要**在 `/usr/bin/data` 建目录——0.1.17 曾因此在 deb 安装下 PermissionDenied 直接 panic。
> - 显式覆盖：环境变量 `CAPILOT_HOME`。
> - 克隆/打开项目/fs 白名单：允许任意本地盘普通目录；拒绝 `Windows` / `Program Files` / `ProgramData` 等系统路径（不再锁死 `$HOME`）。
> - NSIS：`installMode: both`（当前用户 / 全部用户可选）。当前用户安装数据落在 `<安装目录>/data`；全用户安装因目录不可写回退 `~/CaPilot`。

> 已落地（2026-08-18）：**安装包附带官方主题资源**。
> - `tauri.conf.json` → `bundle.resources` 把仓库根 `themes/` 映射到包内 `$RESOURCE/themes/`（JSON 色板 + `wallpapers/` 壁纸）。
> - **Windows NSIS / 便携**：`$RESOURCE` 通常即安装目录，装完可在 exe 旁看到 `themes/`。
> - **Linux deb / AppImage**：资源在包的 resource 树里（常见 `/usr/lib/<app>/…` 或 AppImage 只读镜像内），**不在** `/usr/bin`。
> - **运行时仍读前端 Vite 内置 bundle**（`ui/state/themes.ts` 的 `import.meta.glob`）；磁盘上的 `themes/` 供检视与后续扩展，升级安装可能覆盖该目录。
> - 用户自定义主题（未做）：规划路径 `<data_root>/themes/`，与只读官方资源分离，避免 Program Files / deb 不可写。
>
> 已落地（2026-08-20）：**主题编辑器进入构建版本**。
> - 浮动主题编辑器不再被 `import.meta.env.DEV` 裁掉；`pnpm tauri build` 会打进生产包。
> - 设置 → 外观 → 「显示主题编辑器」开关（默认关，写入 `localStorage` `capilot.themeLab.enabled`）。Ctrl+Shift+T 同步翻转该开关。
> - 标注工具仍仅 `tauri dev`。
> - 保存：开发态仍覆盖仓库 `themes/<id>.json`；安装包只读 `$RESOURCE/themes/`，写入 `<data_root>/themes/`（运行时目录仍读 Vite 内置 glob，磁盘副本供导出 / 后续热加载）。
>
> 已落地（2026-08-20）：**终端随机名称改为 JSON 名称库**。
> - 仓库根 `name-packs/*.json` 经 `bundle.resources` 打进 `$RESOURCE/name-packs/`（安装目录旁可见）。
> - 运行时还扫 `<install>/name-packs/` 与 `<data_root>/name-packs/`；同 id 后者覆盖前者，方便用户自己加一份。
> - 设置 → 外观 → 终端名称库 切换；选中的 id 写入 settings KV `name_pack`（缺省 `tica-cats`）。
> - 设置里可「导入文件」或「粘贴 JSON」。写入 `<data_root>/name-packs/<id>.json` 并立刻启用。
> - JSON 形状：`{ "id", "name", "note", "names": ["…"] }`，或裸数组 `["甲","乙"]`（id 取文件名；粘贴时为 `pasted`）。编译期仍内嵌 TICA 列表作回退。

> 已落地（2026-08-18）：**关闭确认仅在有存活 PTY 时弹出**。
> - `handleTitlebarClose`（`ui/state/exitDaemon.ts`）：设置仍为「询问」时，若没有任何 live agent PTY（无终端 / 全休眠 / 全 ended），直接关窗，不弹 `ExitDaemonDialog`。
> - 「存活」判定：`agentChannels` 有 channel 且 agent 状态不是 `done`/`failed`。`sleepProject` 会清 channel，自然退出的 done 会话可能保留死 channel 仅作 scrollback——均不算存活。

> 已落地（2026-08-18）：**默认权限 = 全开 + Codex/Claude 危险 flag 对齐当前 CLI**。
> - 前端默认 `permissionMode: "yolo"`；Rust `agent_spawn` 缺省 mode 同步为 `yolo`。
> - Codex yolo：`--dangerously-bypass-approvals-and-sandbox`（0.147+ 已无短别名 `--yolo`；**不可**与 `--ask-for-approval` / `--sandbox` 同用）。
> - Claude yolo：`--dangerously-skip-permissions`（不再叠 `--permission-mode bypassPermissions`）。
> - Settings → 已安装 → ⚙ 的 args override 若**已含**危险/权限 flag，`apply_launch_overrides` **不再**重追加 `mode_args`（曾导致 clap 互斥：override 写 bypass + 自动补 ask 参数 → 进程秒退）。
> - 设置框里的 `DEFAULT_LAUNCH`（如 codex `--no-alt-screen`）只是预填展示，**不是**完整启动 argv；完整参数 = adapter + 当前权限模式 + hook 注入。
> - 事实表见 `ai-runtime-references.md` §2.1 / §2.2 / 表项 6、17。

> 已落地（2026-08-18）：**Windows status hook 用绝对 sh 路径**。
> - 现象：Codex 终端 `SessionStart/UserPromptSubmit hook (failed) exit 1`——profile 写 `command = "/bin/sh A:\…\hook.sh"`，Codex hook 子进程 PATH 上没有 `/bin/sh`。
> - 修复：`status_hooks::resolve_posix_sh()` 在 Windows 解析 Git 的 `sh.exe`；`write_status_profile` / `hooks.json` 均写 **绝对 sh + 绝对 hook.sh**（带引号）。
> - 相关但非 CaPilot：`~/.codex/config.toml` 里 Codex 桌面版写入的 `[mcp_servers.node_repl]` 若指向已删除的 `cua_node/<旧hash>/…`，会刷 `MCP client for node_repl failed … 系统找不到指定的路径`——删掉该段或改到新 hash 即可；与 hook-trust 黄字（CaPilot 注入 status hook 的预期警告）无关。
> - Windows 沙箱首次向导（`Set up the Codex agent sandbox` 1/2/3）是 Codex OS 级沙箱安装，与会话权限 flag 不是一层；官方：`[windows] sandbox = "elevated"|"unelevated"`。

> 已落地（2026-08-18）：**Windows WebView2 应用内拖拽**。
> - 根因：`ui/main.tsx` 无条件 `document dragover/drop preventDefault` 在 WebView2 上会把**应用内** HTML5 DnD 的 `dropEffect` 钉成 `none` → 一拖就 🚫（todo / 文件树 / tab 全挂）。Linux WebKitGTK 仍需要该 preventDefault 才能接住 OS 文件拖入。
> - 修复：全局监听**仅**对外部文件拖（`types` 含 `Files` 等）`preventDefault`；应用内拖放交给各目标自己的 handler。
> - **Todo 待分配 tag** 与 **文件树路径**：改为 **pointer 拖拽**（不依赖 HTML5 DnD）。松手时分别命中 `[data-todo-drop-agent]` / `[data-path-drop=composer|terminal]`；路径拖用 `capilot:path-drop` 自定义事件通知 Composer（插 `@路径`）与终端（插 shell-escaped 路径）。
> - 关键文件：`ui/main.tsx`、`ui/components/layout/TodoPanel.tsx`、`ui/components/layout/RightSidebar.tsx`（FilesPanel）、`ui/state/dropPaths.ts`、`Composer.tsx` / `XTermPanel.tsx` / `LeftSidebar.tsx` / `TabBar.tsx`。

> 已落地（2026-08-18）：**F1 焦点切换 + 完成提示音/闪烁 + todo 自动进待处理**。
> - **F1**：焦点在终端时 xterm `attachCustomKeyEventHandler` 会吞掉 F1（`return false` → stopPropagation），Composer 冒泡阶段监听收不到。改为 **捕获阶段** `window.addEventListener("keydown", …, true)`（`Composer.tsx`）。
> - **提示音**：WebView AudioContext 常 `suspended`；`sound.ts` 在首次 pointer/key 解锁，resume 后再播。设置键 `sound_enabled`。
> - **Tab 闪烁**：`tabFlash` 以 agentId 为键，DOM 以 `tab.id` 注册——解析时同时匹配 `tab.id` 与 `tab.agentId`（`TabBar.tsx`）。
> - **Todo 卡在 assigned / 无提示音**：完成逻辑原依赖 hook `working→idle`，1s 轮询常漏掉短暂 `working`。新增 `turnPending`（`markAgentSubmitted` 开启）：在提交后窗口内，看到 terminal idle/dormant 且（明确 edge / 提交后有活动再安静 / 或 ≥8s）即完成——移动 assigned→待处理、chime、flash。会话 `done`/`failed` 仍会完成。
> - 关键：`ui/state/store.ts`（`turnPending` / `setHookStatus` / `notifyAgentTransition`）、`ui/state/sound.ts`、`Composer.tsx`、`TabBar.tsx`。

### 待开发项

- **编辑器外部改动监视**（notify → 前端刷新）：Git 面板已用 2.5s 前端轮询兜底，编辑器标签页本身仍未监听磁盘改动
- **运行时磁盘主题**：从 `$RESOURCE/themes` + `<data_root>/themes` 加载/覆盖内置主题（本轮仅分发资源，未接线）
- **设置页展示完整默认 argv**：Settings → 已安装 → ⚙ 的 `DEFAULT_LAUNCH` 仍是预填片段，易与真实 adapter 启动行混淆
- **Codex 桌面配置漂移**：用户级 `~/.codex/config.toml` 的 MCP/`notify` 路径随 Codex 桌面升级 hash 变化，CaPilot 不代管；可考虑启动时探测并提示

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
