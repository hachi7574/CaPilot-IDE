# CaPilot IDE — 运行与维护手册

> **日期:** 2026-08-06
> **定位:** 项目的「如何跑 / 已知坑 / 文档地图」运行手册。
> 项目架构、模块划分、交互规格与开发进度见 [CaPilot-IDE-DevPlan.md](CaPilot-IDE-DevPlan.md)（v2.1，已同步实施进度）；产品需求见 [CaPilot-PRD.md](CaPilot-PRD.md)（v3.1）；安全细节见 [security-review.md](security-review.md)。
> 本文不重复上述文档的架构论述，只收录项目运行维护所需、且未在其它文档中的可操作性信息（运行命令、设计资源位置、已知坑、安全注意）。

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

IDE 遵循 CaPilot 主仓库的 **LUCY styleguide**（8-bit Pixel × Apple Smooth，深色科技 + 紫色强调）。完整规范见 `docs/styleguide/ui-style-guide.md`（附 `demo.html` / `logo-preview.html` / `preview.png`）。

要点速查：

- **色彩**：`--bg #07090F`、`--brand #8B5CF6`（紫），状态色仅绿/黄/红
- **字体**：Silkscreen（像素标签）/ PixelifySans（标题）/ Tektur（正文）/ JetBrainsMono（技术）
- **边框**：2px 实线 + 硬阴影 `4px 4px 0`，几乎无圆角
- **动效**：Apple 曲线 `cubic-bezier(0.25, 0.1, 0.25, 1)`

**运行时资源位置：**

- 字体内嵌于 `public/fonts/`：`JetBrainsMono-{Regular,Bold}.ttf`、`PixelifySans-Medium.ttf`、`Silkscreen-Regular.ttf`、`Tektur-{Regular,Medium}.ttf`
- logo 在 `public/*.png`（`logo.png` / `logo-full.png` / `logo-inverted.png` / `logo-rounded.png`）
- 颜色令牌定义在 `ui/App.css :root`（`@font-face` 引用 `/fonts/*.ttf`，全本地、无 Google Fonts）
- 应用图标由主仓库 logo 生成于 `src-tauri/icons/`

**同步规则：** `docs/styleguide/` 与 `docs/Assets/` 是主仓库 `Doc/styleguide/`、`Doc/Assets/` 的复制品，**改设计需两边同步**。同理 `docs/CaPilot-PRD.md` 也复制自主仓库 `Doc/CaPilot-PRD.md`，改文档需两边同步。

## 3. 已知问题与技术债

### 已知技术债（Medium/Low，均未修）

- `.lock().unwrap()` 毒化处理（多处 std Mutex）
- dispatcher `reports` 日志无界增长（需环形缓冲）
- `git_status` 未跟踪大文件整读入内存（应流式）
- `resolve_worker` 前缀匹配歧义（短 id 可能派错 worker）
- ESP `connected` 事件未带 `kind` 字段（前端 fallback 到 BLE）
- `Persistence::open` 启动 expect（`$HOME` 不可写会 panic）

> 已解决（2026-08-06）：「会话 permissionMode 未持久化」已在会话生命周期改造中一并完成 —— mode/speed/model 持久化进 `sessions` 表，Composer 三设置跟随当前会话，详见 DevPlan §6.3。

### 待开发项（DevPlan P3 剩余，详见 DevPlan §8/§9/§7.2）

- **ESP**：USB（`UsbSerial`）/ WiFi（`WifiWs`）传输、配对向导、5s 心跳 / 15s 超时、控制帧 ack/重试
- **语音链路**（最重）：ESP mic → Opus → BLE → Rust 解码 → sherpa-onnx 流式 STT → 实时字幕 → 回复 TTS
- **编辑器外部改动监视**（notify → 前端刷新）：Git 面板已用 2.5s 前端轮询兜底，编辑器标签页本身仍未监听磁盘改动

## 4. 安全注意事项

> 完整细节见 `docs/security-review.md`（CSP / capabilities / 路径白名单 / IPC 暴露逐条 + 发布前 checklist）。

- **信任边界**：`agent_write` / `esp_send` 是高权限命令，信任边界是「打包的前端受信任」——单窗口设计下 XSS 即完全控制应用，靠纵深防御缓解（严格 CSP、无远程内容）。
- **范围收紧（发布前）**：`fs_*` / `git_*` 范围限制建议发布前收紧（git 命令接受任意 `repo` 路径、`fs_write` 可写 `$HOME` 任意处，含 dotfile）；`fs_write` 存在 symlink 逃逸的 fallback bug（见 security-review §3）。
- **updater 占位**：updater 配置是占位 endpoint/pubkey；空 `pubkey` 会跳过签名校验，**发布前必须填真实 HTTPS endpoint 与签名公钥**。

## 5. 文档索引

| 文档 | 位置 | 内容 |
| --- | --- | --- |
| 本手册 | `docs/CaPilot-IDE-RUNBOOK.md` | 运行 / 已知坑 / 文档地图 |
| 开发计划 | `docs/CaPilot-IDE-DevPlan.md` | DevPlan v2.1（架构 / 模块 / 交互 / 里程碑 / 决策清单，已同步实施进度）|
| 产品需求 | `docs/CaPilot-PRD.md` | PRD v3.1（源：主仓库 `Doc/CaPilot-PRD.md`）|
| 安全审查 | `docs/security-review.md` | CSP / 权限 / 路径 / IPC 审查与发布前 checklist |
| LUCY 风格 | `docs/styleguide/` | 设计规范（源：主仓库 `Doc/styleguide/`）|
| 界面预览 | `docs/CaPilot-IDE-Preview.html` | 参考 UI 设计（浏览器打开）|
