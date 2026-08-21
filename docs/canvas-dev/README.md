# 画布实现步骤 — 索引

> **日期:** 2026-08-20
> **分支:** `feature/canvas`
> **产品:** [../canvas-view.md](../canvas-view.md)
> **语义:** [../canvas-semantic-contract.md](../canvas-semantic-contract.md)
> **总览（旧单文件，已拆）：** [../canvas-dev-steps.md](../canvas-dev-steps.md)

一期一个文件。打开对应期的文件，从上往下做；每步有改哪些文件、贴什么、怎么验。**上一步没绿不要开始下一步。**

| 文件 | 期 | 交付 | 对外能说 |
| --- | --- | --- | --- |
| [phase-1a.md](phase-1a.md) | **1a** | 空画布 tab + TabBar 右侧切换 | 「能切到画布」 |
| [phase-1b.md](phase-1b.md) | **1b** | 投影卡片 + 位置持久化 + 双击回 tab | 「空间化 session switcher」 |
| [phase-2.md](phase-2.md) | **2** | 连线 + 从根运行（task / exit code） | 「图可执行」 |
| [phase-3.md](phase-3.md) | **3** | service 就绪 + 端口租约 | 「Run 与图分离」 |
| [phase-4.md](phase-4.md) | **4** | Agent 控制台 + MCP 建图（需确认） | 「Agent 是控制台」 |
| [phase-5.md](phase-5.md) | **5** | 模板 / 快捷 1–5 / CanvasArrangement | 最后做 |
| [create-motion.md](create-motion.md) | **运动学** | 新建终端对齐 cleancode（先修入图，再物体/镜头弹簧） | 「新卡弹出 + 镜头跟过去」 |

「只有期 1」不要对外宣称「有画布工作流」。未做期 2 不要做期 5。

---

## 钉死的决策（各期共用，不要再讨论）

| 决策 | 选择 |
| --- | --- |
| 产品形态 | 多 harness 标签页 IDE + 工作区级画布 tab，**不是** canvas-first |
| 主入口 | TabBar 右侧按钮：终端视角 ↔ 画布视角。单例 `Tab.type = "canvas"` |
| 次入口 | LeftSidebar 项目菜单「打开画布」 |
| 身份 | `projectId + workspaceId + objectKind + objectId`；React Flow `node.id` 不是主键 |
| 图 vs Run | 图只存结构；端口 / plan / exit / 就绪只活在 Run |
| 积木 | 终端积木 = shell/bash PTY（`spawnBashAt`）；鼠标 TUI agent **不是**积木 |
| Agent | 控制台卡片，**不进依赖边** |
| 持久化 | `data_root`，走 Tauri command；禁止 localStorage、禁止前端直接写盘 |
| 库 | 期 1 自研 pan/zoom；期 2 才加 `@xyflow/react`；不上 tldraw / Excalidraw |
| Composer | 发送只认 `agent` tab，不认 canvas |
| 权限 | 图写 / 跑停视同 `agent_write` / Git 级 IPC |

---

## 现有代码锚点

```
ui/App.tsx                          .app-body → RightSidebar | MainArea | LeftSidebar
ui/components/layout/MainArea.tsx   TabBar + ContentArea + Composer
ui/components/layout/TabBar.tsx     标签条；切换按钮加在 .tab-add 与 .tab-win-controls 之间
ui/components/layout/ContentArea.tsx  Panel 按 Tab.type 分支（约 18–46 行）
ui/state/store.ts:327               Tab 接口
ui/state/openFile.ts                fileTab — 新 kind 的范本
ui/state/agentActions.ts            spawnAgent / spawnBashAt 已经 addTab({ type: "agent" })
ui/state/shellPath.ts:89            isShellRuntime
ui/state/session.ts:64              addTabSilent 恢复 agent tab；画布 tab 不要走这里
ui/components/layout/LeftSidebar.tsx:280  openAgentTab
src-tauri/src/lib.rs:44             sanitize_project
src-tauri/src/lib.rs:4547           generate_handler!
src-tauri/src/persistence.rs:562    project_dir
src-tauri/capabilities/default.json 不按 command 名允许；新 #[tauri::command] 注册即可
```

TabBar 现状：

```
.tab-bar
  ├── .tab-item × N
  ├── .tab-add                 "+" 新建终端
  └── .tab-win-controls        仅左侧栏收起时（min/max/close，现 margin-left: auto）
```

`addTab`（`store.ts:1859`）按 `id` 去重并激活。同 id 再 `addTab` = 替换 + 激活，不会出现第二张。

---

## 每期都要做的横切

```bash
pnpm tsc --noEmit
cd src-tauri && cargo test
# 改了 IPC / UI 之后：pnpm tauri dev，用测试仓库手动点
```

- 测试仓库：`/home/hachi/Project/capilot_ide_git_test/`（可折腾）
- 本机 Wayland：不要指望截屏/键鼠注入。验收 = 代码走读 + `tsc` / `cargo test` + 作者手动点
- 新 command 的 path 走 `sanitize_project` + workspace allow-list
- 不要把 graph JSON 塞进 `setting_set`
- 不要在 `appendAgentOutput` 热路径里触发画布 re-render
- 禁止全屏画布 CSS animation（xterm cursor blink 教训，`App.css:345`）
- GUI 禁止 emoji；图标走 `ui/assets/icons/` + `<Icon name="…" />`

不要做：整页换成 React Flow；「画布 / 编辑器」双主壳；期 1 上 xyflow/tldraw；live 端口写进图；卡片内嵌 TUI；鼠标 TUI 当 DAG 积木；持久化流程 ID；Composer 认 canvas；emoji。
