# ACP Dev Status

updated_at: 2026-08-17T06:17:00+08:00
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
- [x] `cargo test --lib` 相关 acp 全绿；`cargo test acp` **24**（含真实 opencode smoke）
- [x] `pnpm tsc --noEmit` 通过
- [x] 双轨：PTY adapters 未迁

### product_gate（产品 — **不可 EXEMPT**）
- [x] **真 `opencode acp` prompt 成功一轮**（smoke model=go flash）
- [x] Composer **显示模型菜单**（代码路径）
- [x] 限流错误 **人话** + **自动 fallback**（代码路径；检测扩 429/quota）
- [x] **静默路径已修（DEF-011a/b）** — 代码交付；真 UI 手点仍待
- [ ] **用户在 CaPilot UI 手点一轮成功**（须加载本 worktree 构建，非 `/usr/bin` 04:56）
- [ ] 思考强度按钮在 ACP 下可用（待 UI 手点）

**goal_met 条件：** eng_gate ∧ product_gate（含用户 UI 手点成功或书面签字）。  
**禁止**再用「代码路径 PASS / 真 UI EXEMPT」把 goal_met 打 true。

## Phase checklist
### Phase 0–4
- [x] 工程交付完成
- [x] ~~goal_met=true~~ **已撤销**

### Phase 5 — 产品可用性
- [x] bootstrap + fallback + Composer 模型/思考 + smoke（`3c0875848`）
- [x] **Test 06:20 DEF-011 独立证伪** — `docs/acp-dev-log/20260817-0620-test.md`
- [x] **DEF-011 代码修复**（静默 / live / auto-resume / go 默认）→ dev_done 06:17
- [ ] Test 复测 + 用户 UI 手点补签

## Open defects
| id | phase | severity | title | status |
| --- | --- | --- | --- | --- |
| DEF-011 | 5 | **blocker** | 真 UI 发消息无助手回复 | **mitigated in code**（待 Test/用户装载新构建验证） |
| DEF-011a | 5 | blocker | handleSend 静默 + 乐观气泡先于 invoke | **fixed** |
| DEF-011b | 5 | blocker | emptyAcpSession.live 默认 true；死 host 不 resume | **fixed** |
| DEF-011c | 5 | major | 系统包二进制偏旧 | **open**（用户侧装载；Dev 交付 debug 路径） |
| DEF-010 | 5 | minor | Zen free 本机 Console rate limit | open（默认已改 go flash；fallback 仍在） |
| DEF-008 | 5 | blocker | 真机 prompt rate limit | mitigated |
| DEF-009 | 5 | major | ACP 无模型/思考 UI | mitigated in code |

### DEF-011 代码修复摘要（06:17）
1. `emptyAcpSession().live` 默认 **false**
2. `ensureAgentChannel` → `acp_session_alive`；死才 resume；失败面板+notify
3. `sendPromptToAgent`：**先** ensure+`acp_prompt`，成功后再 user 气泡；sync 失败上屏
4. `Composer.handleSend` catch → notify + 面板 error
5. bridge `contains/is_alive` 剔除死进程 + emit 人话
6. `acp_prompt` 无活进程自动 resume；DB zen free → pin go flash
7. catalog/bootstrap 默认 **opencode-go/deepseek-v4-flash**
8. 新增 `acp_session_alive` 命令

## Human notes
- **禁止**改 LeftSidebar 用户改动。
- **禁止** Dev/Test → Watcher notify。
- 用户须 `pnpm tauri dev`（本 worktree）或安装 `target/debug/capilot-ide`；系统包 04:56 无本修复。
- goal_met=false 直到产品门禁。

## Last handoff（中文）
- **06:17 Dev → Test：** DEF-011a/b 代码修完；默认 go flash；acp_prompt 自动 resume；cargo acp **24**；smoke go flash pong；tsc 0。log `20260817-0616-dev.md`。goal_met=false。请复测。

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev | 2026-08-17T06:17 | idle | ad172813；DEF-011 代码 dev_done，已 send Test |
| test | 2026-08-17T06:20 | idle | d62ccd50；此前证伪，待收 dev_done 复测 |
| watcher | 2026-08-17T06:09 | idle | ed60735f；20m 巡检 |
