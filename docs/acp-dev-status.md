# ACP Dev Status

updated_at: 2026-08-17T13:03:00+08:00
goal_met: false
current_phase: 5
phase_gate: test_passed
owner_lock: none
blocked_reason: "代码 test_passed + tauri dev 已起；缺用户在 dev 窗口手点。Watcher opencode-go 月额度耗尽，编排者会话 cron 代巡检"
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
- [x] 本 worktree `pnpm tauri dev` / debug GUI **进程已起**（Test 06:25 核对）
- [ ] **用户在 tauri dev 窗口对手点一轮有助手回复**（或书面签字）
- [ ] 思考强度 ACP 手点（次要）
- [ ] DEF-011c：用户确认未用 `/usr/bin` 04:56 旧包

**goal_met：** eng ∧ product（含 UI 手点）。**禁止**仅代码/进程 PASS 打 true。

## Phase checklist
### Phase 0–4
- [x] 工程交付；~~goal_met~~ 已撤销

### Phase 5 — 产品可用性
- [x] phase5 初版 `3c0875848`
- [x] Test 06:20 DEF-011 证伪
- [x] Dev `6d6e5b0ab` DEF-011 修复
- [x] Test 06:30 代码复测 → **test_passed**（goal_met=false）
- [x] Dev 06:24 `pnpm tauri dev` + RUNBOOK（`bca950607`）
- [x] Test 06:25 现场核对：debug GUI pid 在跑 / 系统包仍旧
- [ ] **用户 UI 手点补签** → 关 DEF-011 / goal_met

## Open defects
| id | phase | sev | title | status |
| --- | --- | --- | --- | --- |
| DEF-011 | 5 | blocker | 真 UI 无助手回复 | **mitigated in code**；通道已备；待手点关闭 |
| DEF-011a | 5 | blocker | 发送失败不上屏 / 乐观气泡 | **fixed** |
| DEF-011b | 5 | blocker | live 默认 true / resume 短路 | **fixed** |
| DEF-011c | 5 | major | 旧系统包 | **mitigated**（tauri dev 已起+RUNBOOK）；待用户点对窗口 |
| DEF-010 | 5 | minor | Zen free 限流 | open（默认 go flash） |
| DEF-008/009 | 5 | — | rate limit / 模型 UI | mitigated in code |

## Human notes
- **正在跑：** worktree `pnpm tauri dev` → vite `:1420` + `target/debug/capilot-ide` **pid 168359**。日志 `/tmp/capilot-tauri-dev-def011.log`。
- **手点：** 聚焦 **tauri dev 弹出的窗口**（不要系统旧图标）→ `acp:opencode` → Composer 发 `ping` → 要有助手回复或人话错误。
- 禁止 `/usr/bin/capilot-ide`（04:56）。禁止 Dev/Test→Watcher。禁止改 LeftSidebar。
- **goal_met=false** 直到手点成功或书面签字。

## Last handoff（中文）
- **11:47 Watcher 巡检：** phase=5, gate=test_passed（代码），**goal_met=false**。Dev/Test 均 idle；`target/debug/capilot-ide` pid 168359 **仍在跑**（elapsed ~5h23m）。无新 commit。唯一剩余项=**用户 UI 手点**（product_gate / DEF-011 关票）。不 send 双角（空转）。
- **用户手点指引：** 聚焦 **tauri dev 弹出的窗口**（勿点 `/usr/bin` 04:56 旧包）→ `acp:opencode` → Composer 发 `ping` → 需有助手回复或人话错误。
- goal_met 保持 false，直到手点成功或书面签字。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T06:26 | idle | ad172813；交付完毕，tauri dev 在跑（pid 168359），等用户手点 |
| test | 2026-08-17T06:25 | idle | d62ccd50；代码复测 test_passed，等手点关票 |
| watcher | 2026-08-17T11:47 | idle | ed60735f；等用户手点，不拉双角 |
