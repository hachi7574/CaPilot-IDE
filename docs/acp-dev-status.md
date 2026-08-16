# ACP Dev Status

updated_at: 2026-08-17T05:35:00+08:00
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
- [x] `pnpm tsc --noEmit` 通过（Phase2 复验）
- [ ] 文档：附录 A（OpenCode 摸底 + 验收记录）；ai-runtime-references / RUNBOOK 有链接
- [ ] （延伸）第二个 ACP agent 仅加 descriptor 可冒烟

## Phase checklist
### Phase 0 — OpenCode ACP 协议摸底
- [x] `opencode acp` initialize → session/new → prompt 日志
- [x] 记录 capabilities / permission / loadSession / cancel（附录 A.2）
- [x] 工具类 prompt 观察 tool_call / fs / permission
- [x] mock fixture `src-tauri/tests/fixtures/mock_acp_agent.py`
- [x] **Test 2026-08-17T04:10：** mock 独立 NDJSON + 锚点复验（cancel=notification）+ cargo/tsc 绿 → `test_passed`

### Phase 1 — 后端 Host MVP
- [x] acp/ 模块（自研 NDJSON Host；未绑 agent-client-protocol crate — 设计允许降级）
- [x] 默认 descriptor 含 opencode → `acp:opencode`
- [x] bridge/host/events/registry；lib.rs 分叉（**is_acp_runtime 先于 get_adapter，DEF-001**）
- [x] `acp_cancel` 对 OpenCode 发 **notification 无 id**（DEF-002）
- [x] mock 测试绿（9）+ 全量 cargo 215 + OpenCode 无 UI cancel/prompt 烟雾
- [x] **Test 2026-08-17T04:55：** 多角度复测 commit `145072f9a` → `test_passed`（见 log）

### Phase 2 — 前端面板 + 输入区 MVP
- [x] runtimeTransport + store/actions（`acpSessions` / `applyAcpEvent` / `markAcpLive`）
- [x] AcpSessionPanel + ContentArea（`isAcpRuntime`，勿用 === opencode）
- [x] Composer ACP 控件集 + cancel；隐藏 PTY OpenCode ⚡/Build/F12 路径
- [x] `acp://event` 全局订阅，按 agentId 入 store（多 tab 隔离）
- [x] tsc 绿
- [ ] **Test 待复测** → UI 烟雾 + §12 D*/C*/F1–F4/F10/F13–F15

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
- [x] Host 层：spawn/prompt/cancel/kill 协议层（mock + OpenCode cancel notification）
- [x] F1–F4/F10/F13 前端路径：spawn/prompt/cancel/kill 经 UI actions（待 Test UI 实跑）
- [ ] F 其余（permission/fs/多 tab UI 实跑）未签字
### 显示 D1–D15
- [x] 代码路径：AcpSessionPanel 分发 + 隐藏 PTY OpenCode 方言（待 Test UI）
### 输入区 C1–C15
- [x] 代码路径：`acp_prompt` / Stop→`acp_cancel` / 禁 ACP `agent_write`（待 Test UI）

## Open defects（Test 写入，Dev 认领）
| id | phase | severity | title | repro | status |
| --- | --- | --- | --- | --- | --- |
| DEF-001 | 1 | major | `get_adapter` 未知 id 默认 Claude；`acp:*` 无前置分叉 | 本轮确认 lib 全路径守卫 | **closed** |
| DEF-002 | 0/1 | blocker（Phase1） | OpenCode `session/cancel` 仅 notification | host Notify 无 id + 真机 cancelled | **closed** |
| DEF-005 | 0/1 | major | mock cancel-with-id；permission 路径不稳 | cancel-with-id→-32601 已确认；permission 仍 Phase3 | **partial**（cancel 支 closed；permission → Phase3） |
| DEF-003 | 0 | major | 附录未填/无成功 prompt | 94efdf896 + Test 复验 | closed |
| DEF-004 | 0 | minor | 默认 model 易限流 | A.2 free 模型 | closed |

## Human notes
- 工作区保留 `ui/components/layout/LeftSidebar.tsx` 用户改动，禁止覆盖。
- 设计基线：`docs/acp-runtime-plan.md`。验证锚点 **OpenCode ACP**（`opencode acp` → `acp:opencode`）。
- PTY `runtime: "opencode"` 与 `acp:opencode` **不得混用**。
- **Dev↔Test 主路径直连 send**；Watcher 每 20m 救援/推进 phase。
- Test **按实际 diff 自定多角度计划**，不照抄文档清单。

## Last handoff（中文）
- **05:35 Dev：** Phase 2 前端 MVP `dev_done`。新增 `runtimeTransport` / `acpTypes` / `acpEvents` / `AcpSessionPanel`；`agentActions` 走 `acp_prompt`/`acp_cancel`；Composer 隐藏 PTY OpenCode 方言 + Stop；ContentArea `isAcpRuntime` 分叉；TabBar connected=`agentChannels|acpSessions.live`。`pnpm tsc --noEmit` 0；`cargo test acp` 9 绿。未改 LeftSidebar。请 Test 直连复测 UI 路径与 §12 D*/C*。
- Phase 3 仍：permission Host 策略 + fs 沙箱 + 真工具。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T05:35 | idle | ad172813；Phase 2 dev_done，已 notify Test |
| test | 2026-08-17T04:55 | idle | d62ccd50；待收 Phase2 |
| watcher | 2026-08-17T05:07 | idle | ed60735f |
