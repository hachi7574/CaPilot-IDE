# ACP Dev Status

updated_at: 2026-08-17T11:20:00+08:00
goal_met: false
current_phase: 5
phase_gate: dev_done
owner_lock: none
blocked_reason: ""
validation_anchor: acp:opencode
watcher_cadence: 20m heartbeat acp-watcher-tick — 产品门禁未过前勿结案

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
- [x] Host+FE 代码/mock：spawn→prompt→流式→permission→cancel→kill
- [x] `cargo test --lib` 全绿；`cargo test acp` 全绿（含真实 opencode smoke，可 skip）
- [x] `pnpm tsc --noEmit` 通过
- [x] 双轨：PTY adapters 未迁

### product_gate（产品 — **不可 EXEMPT**）
- [x] **真 `opencode acp` prompt 成功一轮**（本机证据：`opencode_acp_real_prompt_smoke` + 手工 NDJSON；模型 `opencode-go/deepseek-v4-flash`）
- [x] Composer **显示模型菜单**（`acp:opencode` catalog：Zen free 默认 + Go）
- [x] 限流错误 **人话**（`humanize_acp_error`）；限流时 **自动 fallback** 到 go/deepseek-v4-flash
- [ ] 用户在 CaPilot **UI** 手点一轮（Wayland 无自动化；待用户补签）
- [ ] 思考强度按钮在 ACP 下可用（已接 effort catalog + live set；待 UI 手点）

**goal_met 条件：** eng_gate ∧ product_gate（含用户 UI 手点或书面签字）。  
**禁止**再用「代码路径 PASS / 真 UI EXEMPT」把 goal_met 打 true。

## Phase checklist
### Phase 0–4
- [x] 工程交付完成（见 git log `94efdf896`…`e7a894249`）
- [x] ~~goal_met=true~~ **已撤销**（假完成：真机 rate limit + 无模型 UI）

### Phase 5 — 产品可用性补丁（本轮）
- [x] bootstrap `session/set_config_option` 默认 **Zen free**（`opencode/deepseek-v4-flash-free` 优先）
- [x] registry 填充 ACP 模型/effort 目录；Composer 对 ACP 显示模型/思考菜单
- [x] live `agent_set_session_config` → `AcpBridge::set_model` / effort
- [x] rate-limit 人话 + prompt 自动 fallback → `opencode-go/deepseek-v4-flash`
- [x] 真实 OpenCode smoke 测：`opencode_acp_real_prompt_smoke` **PASS**（go deepseek-v4-flash）
- [ ] 用户 UI 手点补签

## §12 / 产品勾选
- [x] 真 prompt（stdio Host 路径，go 模型）PASS
- [x] 模型切换协议路径 PASS（set_config_option）
- [ ] CaPilot 窗口内手点（待用户）

## Open defects
| id | phase | severity | title | status |
| --- | --- | --- | --- | --- |
| DEF-008 | 5 | blocker | 真机 prompt rate limit / 无成功 chunk | **mitigated**（默认 zen free + go fallback；smoke PASS on go） |
| DEF-009 | 5 | major | ACP 无模型/思考 UI | **mitigated**（catalog + Composer 显示 + live set） |
| DEF-010 | 5 | minor | Zen free 本机仍 Console rate limit | **open**（fallback 到 go；用户额度恢复后 zen 可用） |
| DEF-006/007/001/002/005 | 0–2 | — | — | closed |

## Human notes
- 用户要求：可用 opencode go 或 zen 的 deepseek v4 测；**优先 zen**。实现：bootstrap/UI 默认 zen free；zen 限流时自动 go。
- 本机实测：`opencode/deepseek-v4-flash-free` 仍 Rate limit；`opencode-go/deepseek-v4-flash` **prompt OK**（pong）。
- 保留 `LeftSidebar.tsx` 用户改动。
- 通知拓扑不变：禁止 Dev/Test→Watcher。

## Last handoff（中文）
- **11:20 产品补丁（Phase5）dev_done：**
  - Host bootstrap 读 `configOptions`，`pick_bootstrap_model` 优先 zen free；`session/set_config_option`。
  - `prompt` 遇 rate limit → 自动切 go/deepseek-v4-flash 等并重试。
  - `humanize_acp_error` 限流中文提示。
  - registry：`acp:opencode` 带模型/effort；Composer 对 ACP 显示模型与 ⚡。
  - `agent_set_session_config` 对 live ACP 调 `set_model`/`effort`。
  - 证据：`cargo test --lib opencode_acp_real_prompt_smoke` ok；`cargo test acp` 22+；tsc 0。
  - **goal_met 仍 false**，待用户 UI 手点后可结。
- 用户醒来手点：①新 tab 选 OpenCode (ACP) 应见模型按钮默认 Zen free；②发 ping；若 zen 限流应自动/可选手动切 Go DeepSeek V4 Flash 并出字；③停止/关 tab。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T11:20 | idle | Phase5 product patch dev_done |
| test | 2026-08-17T09:25 | idle | 待复测 product_gate |
| watcher | 2026-08-17T09:47 | idle | 勿再写 goal_met 胜利交接 |
