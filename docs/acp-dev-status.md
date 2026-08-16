# ACP Dev Status

updated_at: 2026-08-17T07:55:00+08:00
goal_met: false
current_phase: 3
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
- [x] 双轨无损：既有 PTY `cargo test` 全绿（Phase1 215+3；Phase2 acp 9；Phase3 acp **18**）
- [x] 安全：默认不写盘、出界读拒绝、permission 默认 ask（代码+单测；OpenCode 进程内工具仍受 OS 用户边界）
- [x] `pnpm tsc --noEmit` 通过
- [ ] 文档：附录 A（OpenCode 摸底 + 验收记录）；ai-runtime-references / RUNBOOK 有链接
- [ ] （延伸）第二个 ACP agent 仅加 descriptor 可冒烟（Picker 已动态列 transport=acp）

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
- [x] OpenCode ACP bootstrap 真机冒烟（initialize+session/new，无付费 prompt）
- [ ] **F5–F9 UI 真机点验** — Wayland 受限；代码路径 + mock 集成测覆盖；OpenCode 进程内工具可能不回调 client permission（Phase0 已知）
- [ ] **Test 复测中**

### Phase 4 — 产品化
- [ ] Settings CRUD / resume（若能力允许）/ usage
- [ ] §12 总表 + PTY 回归 + 附录 A.4 签字
- [ ] RUNBOOK / ai-runtime-references 链接

## §12 验收勾选（实现时维护）
### 功能 F1–F15
- [x] Host 层 spawn/prompt/cancel/kill（Phase1）
- [x] F1 代码路径 PASS（Phase2）
- [x] F2–F4/F10/F13/F14 **代码路径** PASS
- [x] F5–F8 **mock 集成路径** PASS（permission allow/reject + tool status；fs read）
- [ ] F5–F9 **OpenCode 真 UI** — 待 Test/手工；Host 已具备
### 显示 D1–D15
- [x] D1/D14/D15 **代码路径** PASS
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
- **Dev↔Test 主路径直连 send**；Watcher 每 20m 救援/推进 phase。
- Wayland：UI 自动化受限；Phase3 以代码走读+mock 集成+OpenCode bootstrap 为准。
- OpenCode 可能在 agent 进程内跑工具而不调 client `fs/*` / `request_permission`（Phase0）；Host 策略在 agent **确实**回调时生效。

## Last handoff（中文）
- **07:55 Dev → Test：** Phase 3 实现完成，`phase_gate=dev_done`。请复测。
- **交付：**
  1. `acp/fs_sandbox.rs` — 绝对路径 + canonicalize 落在 session cwd；symlink 逃逸拒；写盘禁用；2MiB 帽；单测 6。
  2. `host.rs` — `fs/read_text_file` 沙箱应答；`fs/write_text_file` 硬拒；未知 agent 请求 `-32601`；permission 仍 ask-only。
  3. `bridge.rs` — respond_permission 后 emit `status:running`；集成测 permission allow/reject + fs in/out。
  4. `mock_acp_agent.py` — 单调 req id；`fsread:<path>`；permission outcome 映射 tool status。
  5. `docs/security-review.md` §6 ACP 威胁面。
- **自测：** `cargo test acp` → **18 passed**；`pnpm tsc --noEmit` 0；OpenCode `initialize+session/new` bootstrap OK。
- **请 Test：** 按 diff 自定多角度（permission 状态机、fs 出界、mock 回归、前端卡仍接 `acp_respond_permission`）；OpenCode 真工具 F5–F9 若无法 UI 点验可标豁免/代码路径。
- **勿推进 Phase 4** 除非你判 Phase 3 test_passed。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T07:55 | idle | ad172813；Phase3 dev_done，已直连 Test |
| test | 2026-08-17T06:50 | idle | d62ccd50；待 Phase3 复测 |
| watcher | 2026-08-17T07:00 | idle | ed60735f；已 +phase→3 |
