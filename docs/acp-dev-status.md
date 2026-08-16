# ACP Dev Status

updated_at: 2026-08-17T06:35:00+08:00
goal_met: false
current_phase: 2
phase_gate: dev_done
owner_lock: none
blocked_reason: ""
validation_anchor: acp:opencode
watcher_cadence: 20m heartbeat acp-watcher-tick (cron 7,27,47 * * * * Asia/Shanghai, id d50ec199)

## Agent roster（固定，勿猜）
| role | title | agentId | provider/model |
| --- | --- | --- | --- |
| dev | 开发 | `ad172813-30c1-4bea-82fe-ddfa1e4b0885` | claude/claude-opus-5 bypass |
| test | 测试 | `d62ccd50-e114-4625-a058-6bb75a76dad6` | claude/claude-opus-5 bypass |
| watcher | 监工 | `ed60735f-697e-4222-b470-69557ae6d9e3` | opencode deepseek-v4-flash build |

workspaceId: `wks_d4c2675b95c38ad9`  
cwd: `/home/hachi/.paseo/worktrees/293djwjk/precious-husky`

## Definition of Done（总目标，对应设计 §11）
- [ ] **锚点闭环：** `acp:opencode` UI 完成 spawn → prompt → 流式 → permission → cancel → kill
- [ ] **§12 验收表**（功能 F* / 显示 D* / 输入区 C*）无未豁免失败项
- [ ] 显示：AcpSessionPanel 正确；无 PTY OpenCode 方言控件误显；Tab 状态不乱
- [ ] 输入区：Composer 走 `acp_prompt`；无 `agent_write` 泄漏；停止/多 tab 不串台
- [x] 双轨无损：既有 PTY `cargo test` 全绿（Phase1 复验 215+3；Phase2 复验 acp 9）
- [ ] 安全：默认不写盘、出界读拒绝、permission 默认 ask
- [x] `pnpm tsc --noEmit` 通过（Phase2 复验 + DEF-006 修复复验）
- [ ] 文档：附录 A（OpenCode 摸底 + 验收记录）；ai-runtime-references / RUNBOOK 有链接
- [ ] （延伸）第二个 ACP agent 仅加 descriptor 可冒烟

## Phase checklist
### Phase 0 — OpenCode ACP 协议摸底
- [x] **Test 2026-08-17T04:10：** → `test_passed`

### Phase 1 — 后端 Host MVP
- [x] **Test 2026-08-17T04:55：** commit `145072f9a` → `test_passed`；DEF-001/002 closed

### Phase 2 — 前端面板 + 输入区 MVP
- [x] runtimeTransport + store/actions（`acpSessions` / `applyAcpEvent` / `markAcpLive`）
- [x] AcpSessionPanel + ContentArea（`isAcpRuntime`）
- [x] Composer ACP 控件集 + cancel；隐藏 PTY OpenCode 方言
- [x] `acp://event` 全局订阅按 agentId
- [x] tsc 绿 / cargo acp 9
- [x] **F1 主路径可 spawn `acp:opencode`** ← DEF-006 fixed（TerminalTemplatePicker 动态 ACP 段）
- [x] DEF-007 Host prompt 开始 emit `status:running`
- [x] 侧栏 switch 列表过滤 ACP（不走 `agent_switch_runtime`）
- [ ] **Test 复测中** — commit 见 Last handoff

### Phase 3 — 权限与安全
- [ ] permission 闭环 + 卡 UI（面板已有 MVP 卡；Host 策略/沙箱 Phase3）
- [ ] OpenCode 真工具触发 F5–F9
- [ ] fs/read 沙箱
- [ ] security-review 补丁段落

### Phase 4 — 产品化
- [ ] Settings CRUD / resume（若能力允许）/ usage
- [ ] §12 总表 + PTY 回归 + 附录 A.4 签字
- [ ] RUNBOOK / ai-runtime-references 链接

## §12 验收勾选（实现时维护）
### 功能 F1–F15
- [x] Host 层 spawn/prompt/cancel/kill（Phase1）
- [x] **F1 代码路径** — Picker 动态 ACP → `spawnAgent(proj,"acp:…")`（待 Test UI 实跑）
- [x] F2–F4/F10/F13 **代码路径**（send/cancel/panel/tab isolation）；**UI 实跑**待复测
- [x] F14 后端拒 `agent_write`（Phase1）
### 显示 D1–D15
- [x] D1/D14/D15 **代码路径** PASS；UI 实跑待复测
### 输入区 C1–C15
- [x] C1–C6 **代码路径** PASS（acp_prompt/cancel；方言隐藏；perm 只持久化）；UI 实跑待复测

## Open defects（Test 写入，Dev 认领）
| id | phase | severity | title | repro | status |
| --- | --- | --- | --- | --- | --- |
| DEF-006 | 2 | **blocker** | 无主 UX spawn `acp:opencode` | 项目+无 ACP | **fixed**（待 Test 确认） |
| DEF-007 | 2 | minor | Host prompt 开始不 emit running | host status emit | **fixed**（待 Test 确认） |
| DEF-001 | 1 | major | get_adapter / is_acp 分叉 | — | **closed** |
| DEF-002 | 0/1 | blocker | cancel notification | — | **closed** |
| DEF-005 | 0/1 | major | mock permission 不稳 | Phase3 | **partial** |
| DEF-003/004 | 0 | — | — | — | closed |

## Human notes
- 工作区保留 `ui/components/layout/LeftSidebar.tsx` 用户改动，禁止覆盖。本轮仅在 staged 副本上加 ACP switch 过滤；工作区仍保留用户脏改。
- 设计基线：`docs/acp-runtime-plan.md`。验证锚点 **OpenCode ACP**（`opencode acp` → `acp:opencode`）。
- PTY `runtime: "opencode"` 与 `acp:opencode` **不得混用**。
- **Dev↔Test 主路径直连 send**；Watcher 每 20m 救援/推进 phase。
- Test **按实际 diff 自定多角度计划**，不照抄文档清单。
- Wayland：UI 自动化受限；本轮以代码走读+CLI 回归为主。

## Last handoff（中文）
- **06:35 Dev → Test：** DEF-006/007 已修，`phase_gate=dev_done`，请复测 Phase 2。
- **commit：** 见 git log（`fix(acp): phase 2 — DEF-006 primary spawn UX + DEF-007 running status`）。
- **改动要点：**
  1. `TerminalTemplatePicker`：从 `runtimes` 动态列出 `transport==="acp"` / `acp:*` 且 available → `spawnTerminal` → `spawnAgent(proj, "acp:opencode")`；独立「ACP Agents」分区。
  2. `TermTemplate.runtime` 放宽为 `string`；`runtimeIcon('acp:opencode')→opencode`。
  3. 侧栏 switch 列表 **过滤 ACP**（不调用 `agent_switch_runtime`）；用户 LeftSidebar 其它脏改未提交。
  4. `AcpBridge::prompt` 开始 emit `Status{running}`，失败回 `idle`（DEF-007）。
- **验证：** `pnpm tsc --noEmit` 0；`cargo test acp` 9 passed。
- **请 Test：** 代码走读 F1 主路径 + 回归 panel/composer；确认 DEF-006/007 closed 或重开。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T06:35 | idle | ad172813；DEF-006/007 修完 dev_done，已直连 Test |
| test | 2026-08-17T06:05 | idle | d62ccd50；待复测 |
| watcher | 2026-08-17T06:10 | idle | ed60735f |
