# ACP Dev Status

updated_at: 2026-08-17T06:24:00+08:00
goal_met: false
current_phase: 5
phase_gate: test_passed
owner_lock: none
blocked_reason: "DEF-011 代码 test_passed；缺用户在本 worktree 构建上手点，goal_met 禁止 true"
validation_anchor: acp:opencode
watcher_cadence: 20m heartbeat acp-watcher-tick (cron 7,27,47 * * * * Asia/Shanghai, id c84c327f)

## Agent roster（固定，勿猜）
| role | title | agentId | provider/model |
| --- | --- | --- | --- |
| dev | 开发 | `ad172813-30c1-4bea-82fe-ddfa1e4b0885` | claude/claude-opus-5 bypass |
| test | 测试 | `d62ccd50-e114-4625-a058-6bb75a76dad6` | claude/claude-opus-5 bypass |
| watcher | 监工 | `ed60735f-697e-4222-b470-69557ae6d9e3` | opencode deepseek-v4-flash build |

workspaceId: `wks_d4c2675b95c38ad9`  
cwd: `/home/hachi/.paseo/worktrees/293djwjk/precious-husky`

## Definition of Done（拆成两层）
### eng_gate（工程）
- [x] Host+FE 代码/mock 主路径
- [x] `cargo test acp` **24**（含 `opencode_acp_real_prompt_smoke`）
- [x] `pnpm tsc --noEmit` 0
- [x] 双轨 PTY 未迁
- [x] DEF-011a/b **代码修复**经 Test 复测（`6d6e5b0ab`）

### product_gate（**不可 EXEMPT**）
- [x] stdio/smoke go flash prompt（工程锚点）
- [x] Composer 模型菜单 + 限流人话/fallback（代码路径）
- [ ] **用户加载本 worktree 构建后 UI 手点一轮有助手回复**（或书面签字）
- [ ] 思考强度 ACP 手点（次要）
- [ ] DEF-011c：勿用 `/usr/bin` 04:56 旧包验证

**goal_met：** eng ∧ product（含 UI 手点）。**禁止**仅代码 PASS 打 true。

## Phase checklist
### Phase 0–4
- [x] 工程交付；~~goal_met~~ 已撤销

### Phase 5 — 产品可用性
- [x] phase5 初版 `3c0875848`（zen/fallback/菜单/smoke）
- [x] Test 06:20 DEF-011 证伪
- [x] Dev `6d6e5b0ab` DEF-011 静默/live/go 默认 / auto-resume
- [x] **Test 06:30 代码复测 → test_passed**（goal_met=false）
- [x] Dev 06:24 启动本 worktree `pnpm tauri dev` + RUNBOOK 勿用旧包说明
- [ ] 用户 UI 手点补签 → 关 DEF-011 / goal_met

## Open defects
| id | phase | sev | title | status |
| --- | --- | --- | --- | --- |
| DEF-011 | 5 | blocker | 真 UI 无助手回复 | **mitigated in code**；待 UI 手点关闭 |
| DEF-011a | 5 | blocker | 发送失败不上屏 / 乐观气泡 | **fixed** |
| DEF-011b | 5 | blocker | live 默认 true / resume 短路 | **fixed** |
| DEF-011c | 5 | major | 用户可能仍跑 `/usr/bin` 04:56 | **open**（已起 tauri dev；RUNBOOK 已写明） |
| DEF-010 | 5 | minor | Zen free Console 限流 | open（默认已 go flash） |
| DEF-008/009 | 5 | — | rate limit / 模型 UI | mitigated in code |

## Human notes
- **正在跑：** 本 worktree `pnpm tauri dev`（vite :1420 + `target/debug/capilot-ide`）。日志 `/tmp/capilot-tauri-dev-def011.log`。
- 验证 ACP **禁止** `/usr/bin/capilot-ide`（04:56）。见 RUNBOOK §1「ACP 验证：勿用旧系统包」。
- 禁止 Dev/Test→Watcher。禁止改 LeftSidebar。
- **goal_met=false** 直到手点成功或书面签字。

## Last handoff（中文）
- **06:30 Test → Dev：** `6d6e5b0ab` 代码 **test_passed**；product 待手点。
- **06:24 Dev：** 已 `pnpm build` + **`pnpm tauri dev` 拉起 GUI**；RUNBOOK 补充勿用系统旧包。请用户在 **tauri dev 窗口** 对 `acp:opencode` 发短消息验证。成功前不 goal_met。未 send Watcher。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T06:24 | idle | ad172813；product assist：tauri dev 已起，等用户手点 |
| test | 2026-08-17T06:30 | idle | d62ccd50；代码 test_passed，goal_met=false |
| watcher | 2026-08-17T06:09 | idle | ed60735f；20m 读 status |
