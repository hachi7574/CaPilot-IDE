# ACP Dev Status

updated_at: 2026-08-17T04:05:00+08:00
goal_met: false
current_phase: 0
phase_gate: dev_done
owner_lock: none
blocked_reason: ""
validation_anchor: acp:opencode
watcher_cadence: 20m via paseo-loop on watcher agent

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
- [ ] 双轨无损：既有 PTY `cargo test` 全绿
- [ ] 安全：默认不写盘、出界读拒绝、permission 默认 ask
- [ ] `pnpm tsc --noEmit` 通过
- [ ] 文档：附录 A（OpenCode 摸底 + 验收记录）；ai-runtime-references / RUNBOOK 有链接
- [ ] （延伸）第二个 ACP agent 仅加 descriptor 可冒烟

## Phase checklist
### Phase 0 — OpenCode ACP 协议摸底
- [x] `opencode acp` initialize → session/new → prompt 日志
- [x] 记录 capabilities / permission / loadSession / cancel（附录 A.2）
- [x] 工具类 prompt 观察 tool_call / fs / permission
- [x] mock fixture `src-tauri/tests/fixtures/mock_acp_agent.py`

### Phase 1 — 后端 Host MVP
- [ ] 依赖 + acp/ 模块
- [ ] 默认 descriptor 含 opencode → `acp:opencode`
- [ ] bridge/host/events/registry；lib.rs 分叉
- [ ] mock 测试绿 + 本机 OpenCode 无 UI 一轮 prompt

### Phase 2 — 前端面板 + 输入区 MVP
- [ ] runtimeTransport + store/actions
- [ ] AcpSessionPanel + ContentArea（isAcpRuntime，勿用 === opencode）
- [ ] Composer ACP 控件集 + cancel；隐藏 PTY OpenCode ⚡/Build/F12 路径
- [ ] 勾选 §12.2 D* + §12.3 C* + F1–F4/F10/F13–F15
- [ ] tsc 绿

### Phase 3 — 权限与安全
- [ ] permission 闭环 + 卡 UI
- [ ] OpenCode 真工具触发 F5–F9
- [ ] fs/read 沙箱
- [ ] security-review 补丁段落

### Phase 4 — 产品化
- [ ] Settings CRUD / resume（若能力允许）/ usage
- [ ] §12 总表 + PTY 回归 + 附录 A.4 签字
- [ ] RUNBOOK / ai-runtime-references 链接

## §12 验收勾选（实现时维护）
### 功能 F1–F15
- [ ] （未开始）
### 显示 D1–D15
- [ ] （未开始）
### 输入区 C1–C15
- [ ] （未开始）

## Open defects（Test 写入，Dev 认领）
| id | phase | severity | title | repro | status |
| --- | --- | --- | --- | --- | --- |

## Human notes
- 工作区保留 `ui/components/layout/LeftSidebar.tsx` 用户改动，禁止覆盖。
- 设计基线：`docs/acp-runtime-plan.md`（只实现，不重开架构）。验证锚点 **OpenCode ACP**（`opencode acp` → runtime `acp:opencode`）。
- 编排：`docs/acp-multi-agent-dev-plan.md`；发送正文：`docs/acp-prompts/*.md`。
- PTY `runtime: "opencode"` 与 `acp:opencode` **不得混用**。
- **Dev↔Test 主路径直连 send**；Watcher 每 20m 救援/推进 phase。

## Last handoff（中文）
- **Dev Phase 0 完成（dev_done）。** 实测 opencode 1.18.18 ACP：initialize/session/new/set_config_option/prompt(成功 pong)/tool_call(bash)/cancel(cancelled)/loadSession。附录 A.2 已写。mock：`src-tauri/tests/fixtures/mock_acp_agent.py`。日志：`docs/acp-dev-log/20260817-0405-*`。
- **建议 Test：** 验证 mock 可独立跑、附录与 json 一致、无越权改业务代码；可自定多角度计划。
- **下一步：** Test pass 后进 Phase 1 Host。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T04:05:00+08:00 | idle | phase0 dev_done，已通知 Test |
| test |  | idle | d62ccd50-e114-4625-a058-6bb75a76dad6 |
| watcher |  | idle | ed60735f-697e-4222-b470-69557ae6d9e3 |
