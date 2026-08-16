# CaPilot IDE 版本检测与升级 — 设计文档

> 状态：已确认（2026-08-16）。决策：GitHub Releases 分发 + 启动静默检查 + 仅 IDE 自身。
>
> 实现状态：P1–P5 代码已落地（`update_check` / `update_download_and_install` 命令、设置页更新 UI、
> `.github/workflows/release.yml` + `bump-version.mjs` / `make-manifest.mjs`）。剩余为发布动作：
> 配置 GitHub secrets、打 tag 触发首次 Release（见 §11）。

## 1. 目标

- 前端展示**真实当前版本**（消除硬编码），启动后**静默检查新版本**，有新版本时给出非侵入式提示
- 用户在设置页一键**下载 + 安装 + 重启**，全程带进度
- 打 tag 触发 CI 自动构建多平台安装包、签名并发布到 GitHub Releases，应用从 Release 拉取更新清单
- 不涉及 AI CLI 的版本管理（由各自包管理器负责）

## 2. 现状与差距

| 现状 | 差距 |
|---|---|
| `tauri-plugin-updater` 2.10.1 已依赖并初始化 | `tauri.conf.json` 里 endpoint 是假地址、`pubkey` 为空，未生成密钥 |
| 版本号在 4 处维护（`package.json`/`Cargo.toml`/`tauri.conf.json` + 前端硬编码 `APP_VERSION`） | 前端应运行时读取，去掉硬编码 |
| 设置页已有"关于"区块 | 无更新 UI |
| 有 GitHub remote | 无 CI、无 Release 自动化 |
| `notification`/`store` 插件已接入 | 可直接复用 |

## 3. 架构总览

```
启动 ──(延迟 3s)──▶ frontend: update_check ──▶ Rust command
                                                 │
                                                 ▼
                                   tauri-plugin-updater ──▶ GET https://github.com/.../releases/latest/download/latest.json
                                                 │             (GitHub 302 → Release 资产)
                                                 ▼
                                   比较 semver，返回 { 当前版本, 最新版本, notes, 是否可更新 }
                                                 │
                              ┌──────────────────┴───────────────────┐
                              ▼                                      ▼
                        有新版本 → 设置页红点 + 桌面通知          无新版本 → 静默
                              │
                              ▼
                   设置页「下载并安装」──▶ update_download_and_install(app, Channel<f64>)
                                                  │ 进度事件 → 前端进度条
                                                  ▼
                                     update.download() → install() → 应用重启
```

## 4. 版本单一来源

`Cargo.toml` 里的 `version` 是**唯一权威源**（Tauri 构建时写入安装包元数据）。

- **Rust 侧**：`app.package_info().version` 直接读取，无需配置文件
- **前端侧**：`import { getVersion } from "@tauri-apps/api/app"`，与 Rust 同源
- 删除 `ui/components/layout/SettingsModal.tsx` 的 `const APP_VERSION = "0.1.0"`
- `package.json` / `tauri.conf.json` 的 version 仍保留（构建工具读取），但不再作为显示来源；发布时用脚本统一对齐（见 §9）

## 5. Rust 侧改动（`src-tauri/src/lib.rs`）

新增两个命令 + 一个状态结构：

```rust
#[derive(Serialize, Clone)]
struct UpdateStatus {
    current_version: String,      // 来自 app.package_info().version
    latest_version: Option<String>,
    available: bool,              // latest > current
    notes: Option<String>,        // 发布说明
    published_at: Option<String>,
    target: String,               // 如 "linux-x86_64"，调试用
    installable: bool,            // 非 debug 构建才可安装
}

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    // 用 tauri_plugin_updater::UpdaterExt 构建 updater，调 .check()
    // 网络失败 → Err("无法连接更新服务器: ...")，由前端兜底展示，不阻塞启动
}

#[tauri::command]
async fn update_download_and_install(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<f64>,   // 0..1 进度
) -> Result<(), String> {
    // 先守卫：cfg!(debug_assertions) → Err("开发构建不支持自动安装")
    // 重新 .check() 拿到 Update → .download(|event| 发射进度) → .install()
    // install() 完成后插件内部触发应用重启
}
```

注册进 `invoke_handler`。`update_check` 用 `async` 且内部 catch 所有错误返回友好文案。

**配置改动**（`src-tauri/tauri.conf.json`）：

```json
"plugins": {
  "updater": {
    "endpoints": [
      "https://github.com/hachi7574/CaPilot-IDE/releases/latest/download/latest.json"
    ],
    "pubkey": "<生成的公钥>",
    "windows": { "installMode": "passive" }
  }
},
"bundle": {
  "createUpdaterArtifacts": true,
  "...": "其余保持不变"
}
```

## 6. 前端改动

**`ui/state/store.ts`** — 新增 update slice：

```ts
interface UpdateState {
  currentVersion: string | null;
  checking: boolean;
  status: "idle" | "checking" | "available" | "up-to-date" | "error";
  latestVersion: string | null;
  notes: string | null;
  error: string | null;
  downloading: boolean;
  downloadProgress: number | null;   // 0..1
  installable: boolean;
}
```

Actions：`checkForUpdate()`（幂等，去重并发）、`downloadAndInstall()`、`setAutoCheckUpdate(bool)`。

**`ui/components/layout/SettingsModal.tsx`** — 重做"关于"区块：

```
┌─ 关于 ─────────────────────────────────────┐
│ CaPilot IDE  v0.1.0          ← getVersion()│
│                                            │
│  [检查更新]   ● 发现新版本 v0.2.0        │
│    ┌──────────────────────────────────┐   │
│    │ 发布说明（可折叠）…              │   │
│    └──────────────────────────────────┘   │
│    [下载并安装 ▸]  [稍后提醒]             │
│    ▓▓▓▓▓░░░░░░ 45%                        │
│                                            │
│  ☑ 启动时自动检查更新                      │
└────────────────────────────────────────────┘
```

- 当前版本：运行时读取，替换硬编码
- 状态分支：`checking`（转圈）/ `available`（红点+按钮+进度条）/ `up-to-date`（"已是最新版本"）/ `error`（失败文案 + 重试）
- `installable=false` 时（开发构建）"下载并安装"置灰并 tooltip 提示
- "启动时自动检查更新"开关 → 持久化到 `setting_set` 新键 `auto_check_update`（需加入 `lib.rs` 的 ALLOWED 白名单，默认开启）

**启动自动检查**（`ui/App.tsx` 或 ContentArea 挂载后）：

1. 读 `setting_get("auto_check_update")`，默认 true
2. 延迟 ~3s 调 `checkForUpdate()`（不抢启动渲染）
3. 返回 `available` 时：设置页标红点 + 通过 `tauri-plugin-notification` 发桌面通知「CaPilot v0.2.0 已可用」，并做**会话内去重**（记录已通知的版本，避免每次启动重复弹）

## 7. 签名与安全

1. 本机一次性生成密钥对：`pnpm tauri signer generate -w ~/.tauri/capilot.key`（私钥 + 公钥）
2. **公钥**填入 `tauri.conf.json` → `plugins.updater.pubkey`（提交进仓库）
3. **私钥**只放 CI secrets（`TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_PASSWORD`），**绝不进 git**
4. 客户端每次检查都会校验 `latest.json` 里签名与嵌入公钥匹配，防中间人篡改

## 8. 发布链路（GitHub Actions）

新增 `.github/workflows/release.yml`，`on: push: tags: ["v*"]`：

```
job: version-check       → node .github/scripts/bump-version.mjs --check <tag>
job: build (matrix)      → linux (ubuntu-22.04) / windows / macos-14
                             pnpm tauri build（带 TAURI_SIGNING_PRIVATE_KEY 签名）
                             → softprops/action-gh-release 上传安装包 + .sig
   ↓ needs: build 全部完成
job: manifest            → node .github/scripts/make-manifest.mjs
                             （读各平台 .sig 内容 → 写 latest.json）
                             → 上传 latest.json
```

关键点：
- **签名在构建时产生**：`bundle.createUpdaterArtifacts: true` + `TAURI_SIGNING_PRIVATE_KEY` 环境变量，`tauri build` 会在每个安装包旁生成 `.sig` 文件（不是发布时单独签）
- `latest.json` 的 `signature` 字段 = 对应 `.sig` 文件**内容**（文档明确：路径/URL 不行）
- `.github/scripts/make-manifest.mjs` 汇总：从 release 资产里按平台匹配更新产物（Linux 优先 `.AppImage.tar.gz`、Windows `*-setup.exe`、macOS `*.app.tar.gz`），下载 `.sig` 内容写入 manifest，再上传
- Linux 构建需装 `libwebkit2gtk-4.1-dev` 等系统依赖
- 首次发布流程：配好 secrets → 打 tag `v0.1.1` → 观察 Actions → 下载安装包手动装一次验证 → 之后走自动更新

## 9. 版本号对齐

发布时三处需一致：`Cargo.toml`、`package.json`、`tauri.conf.json`。`.github/scripts/bump-version.mjs` 以 Cargo.toml 为准回写另外两处；CI 第一步 `--check <tag>` 校验一致后再构建。注意 `scripts/` 在 `.gitignore` 里，所以脚本放 `.github/scripts/`。

## 10. 分阶段实施

| 阶段 | 内容 | 依赖 | 可独立验证 |
|---|---|---|---|
| **P1 检测与展示** | 版本单一来源、`update_check` 命令、设置页真实版本显示 | 无 | `pnpm tsc --noEmit` + 启动看设置页 |
| **P2 密钥与配置** | 生成密钥、填 pubkey/endpoint、`createUpdaterArtifacts` | P1 | 手工打 tag 出一版 Release |
| **P3 检查与提示** | store slice、启动自动检查、设置页更新 UI、桌面通知 | P1+P2 | 改 latest.json 版本号模拟新版本 |
| **P4 下载安装** | `update_download_and_install` + 进度条 + 重启 | P2+P3 | 打 v0.1.1 从 v0.1.0 升级实测 |
| **P5 CI 自动化** | release.yml + secrets + 版本对齐脚本 | P2 | 打 tag 全链路跑通 |

## 需要留意的边界

- **Linux 安装方式**：更新器对 AppImage 支持最完整；若用户走 `.deb` 安装，行为依赖插件 deb 支持。设计按 AppImage 为主
- **开发构建**：`update_check` 照常可用（方便联调），但 `install` 加 `cfg!(debug_assertions)` 守卫
- **打断会话**：升级重启会关闭正在跑的 agent PTY，安装前 UI 应提示「正在运行的会话将被关闭」
- **忽略版本**：用户点"稍后提醒"时，可选持久化 `ignored_update_version` 到 settings（设计上默认只做会话内去重，跨启动不弹可后加）

## 11. 首次发布 checklist（需人工执行）

代码已就位，剩下的都是发布动作：

1. **配置 GitHub secrets**（仓库 Settings → Secrets and variables → Actions）：
   - `TAURI_SIGNING_PRIVATE_KEY`：`~/.tauri/capilot.key` 的完整内容
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：`capilot-release-2026`
2. **提交并推送**本分支改动（含 `.github/workflows/release.yml`）。
3. **统一版本号并打 tag**：`node .github/scripts/bump-version.mjs && git tag v0.1.1 && git push origin v0.1.1`。
4. **观察 Actions**：`version-check` → `build`（linux/windows/macos）→ `manifest` 应全部通过。
5. **首次验证**：从 Release 手动下载 Linux AppImage 安装一次；改回旧版后走一次「检查更新 → 下载并安装」，确认 `latest.json` 签名校验通过、应用重启到新版。
6. 之后每次发版：改 `Cargo.toml` 版本 → `node .github/scripts/bump-version.mjs` → 提交 → 打 `vX.Y.Z` tag。

> 私钥文件 `~/.tauri/capilot.key` 切勿提交进仓库；公钥已内嵌在 `tauri.conf.json`。
