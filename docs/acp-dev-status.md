# ACP Dev Status

updated_at: 2026-08-17T09:15:00+08:00
goal_met: false
current_phase: 4
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
- [x] 显示：AcpSessionPanel 正确；无 PTY OpenCode 方言控件误显；Tab 状态不乱（代码路径）
- [x] 输入区：Composer 走 `acp_prompt`；无 `agent_write` 泄漏；停止/多 tab 不串台（代码路径）
- [x] 双轨无损：既有 PTY `cargo test` 全绿（Phase4 acp **20**；PTY 未迁）
- [x] 安全：默认不写盘、出界读拒绝、permission 默认 ask（代码+单测；OpenCode 进程内工具仍受 OS 用户边界）
- [x] `pnpm tsc --noEmit` 通过
- [x] 文档：附录 A.4 签字；RUNBOOK / ai-runtime-references 有 ACP 链接
- [x] （延伸）第二个 ACP agent 仅加 descriptor 可冒烟（Settings CRUD + Picker 动态列 transport=acp）

## Phase checklist
### Phase 0 — OpenCode ACP 协议摸底
- [x] **Test 2026-08-17T04:10：** → `test_passed`

### Phase 1 — 后端 Host MVP
- [x] **Test 2026-08-17T04:55：** commit `145072f9a` → `test_passed`；DEF-001/002 closed

### Phase 2 — 前端面板 + 输入区 MVP
- [x] **Test 2026-08-17T06:50：** → `test_passed`；DEF-006/007 closed

### Phase 3 — 权限与安全
- [x] permission Host 策略闭环（ask 默认；emit PermissionRequest；`acp_respond_permission` allow/reject；status waiting→running）
- [x] fs/read 出界沙箱（`fs_sandbox` + Host `fs/read_text_file` / write 硬拒）
- [x] mock 加固 DEF-005（单调 agent req id；allow/reject 集成测）
- [x] security-review §6 ACP 威胁面补丁
- [x] OpenCode ACP bootstrap 真机冒烟（initialize+session/new，无付费 prompt）— Dev 交付
- [x] **F5–F9 UI 真机点验** — **EXEMPT**（Wayland + OpenCode 进程内工具可能不回调 client；Host+mock 覆盖策略）
- [x] **Test 2026-08-17T08:10：** commit `65b851cf9` → **`test_passed`**；DEF-005 closed

### Phase 4 — 产品化
- [x] Settings CRUD（`acp_list/upsert/remove_agent` + Settings「ACP」分区）
- [x] resume：`session/load` + resume_key（mock 集成测 `bridge_resume_via_session_load`；OpenCode loadSession 见附录 A）
- [x] usage：`usage_update` → 面板 + `AgentInfo.last_usage` 镜像
- [x] §12 总表维护 + 附录 A.4 签字 + PTY 回归说明（cargo test acp 绿；未改 PTY adapters）
- [x] RUNBOOK / ai-runtime-references 链接
- [ ] **Test 复测** → 等 `test_passed`

## §12 验收勾选（实现时维护）
### 功能 F1–F15
- [x] Host 层 spawn/prompt/cancel/kill（Phase1）
- [x] F1 代码路径 PASS（Phase2）
- [x] F2–F4/F10/F13/F14 **代码路径** PASS
- [x] F5–F8 **mock 集成路径** PASS（permission allow/reject + tool status；fs read）
- [x] F5–F9 **OpenCode 真 UI** — **EXEMPT**（Wayland；进程内工具 Phase0 已知）；Host 已具备
- [x] F11 resume **mock 路径** PASS（session/load + resume_key）
- [x] F12 Settings CRUD **代码路径** PASS
### 显示 D1–D15
- [x] D1/D14/D15 **代码路径** PASS
- [x] D9 usage **代码路径** PASS（面板 + last_usage）
### 输入区 C1–C15
- [x] C1–C6 **代码路径** PASS

## Open defects（Test 写入，Dev 认领）
| id | phase | severity | title | status |
| --- | --- | --- | --- | --- |
| DEF-006 | 2 | blocker | 无主 UX spawn acp:opencode | **closed** |
| DEF-007 | 2 | minor | Host prompt 不 emit running | **closed** |
| DEF-001 | 1 | major | is_acp 分叉 | **closed** |
| DEF-002 | 0/1 | blocker | cancel notification | **closed** |
| DEF-005 | 0/1 | major | mock permission 不稳 | **closed**（单调 req id + allow/reject 测绿） |
| DEF-003/004 | 0 | — | — | closed |

## Human notes
- 工作区保留 `ui/components/layout/LeftSidebar.tsx` 用户改动；本 phase **未改** LeftSidebar。
- 设计基线：`docs/acp-runtime-plan.md`。锚点 **acp:opencode**。
- PTY `opencode` 与 `acp:opencode` 不得混用。
- **通知拓扑：** 只允许 Dev↔Test 与 Watcher→Dev|Test（救援）。**禁止 Dev/Test→Watcher。**
- Test 在 test_passed 且出口满足时应自己 +phase 再 send Dev；Dev 不要等 Watcher。
- Watcher 每 20m 只读 status 救援；不期待收件箱。
- Wayland：UI 自动化受限；Phase3/4 以代码走读+mock 集成+OpenCode bootstrap 为准。
- OpenCode 可能在 agent 进程内跑工具而不调 client `fs/*` / `request_permission`（Phase0）；Host 策略在 agent **确实**回调时生效。

## Last handoff（中文）
- **09:15 Dev Phase 4 dev_done：** Settings CRUD UI + 后端命令；resume mock 测；usage→last_usage；RUNBOOK/ai-runtime-references/附录 A.4；`cargo test acp` **20 passed**；`tsc` 0。已直接 send Test 复测。
- Phase 3 已通过（`65b851cf9`）；F5–F9 OpenCode 真 UI 已 EXEMPT。
- 风险：mock 绿 ≠ 锚点 UI 全闭环；goal_met 仍需 Test 勾选 + 可选真机。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T09:15 | idle | ad172813；Phase 4 dev_done，已 send Test |
| test | 2026-08-17T08:10 | idle | d62ccd50；待收 Phase4 复测任务 |
| watcher | 2026-08-17T08:27 | idle | ed60735f；上次救援 send Dev 开工 Phase 4 |
