# ACP Runtime — 多 Agent 无人值守开发计划

> **日期:** 2026-08-17  
> **状态:** 可执行编排稿  
> **目标:** 按 [`acp-runtime-plan.md`](./acp-runtime-plan.md) 交付 Phase 0→4，全程**不依赖用户盯屏**，由 Dev / Test / Watcher 三角色循环推进直到成功标准达成。  
> **分支:** `analyze/acp-agent-runtime-vs-current`（或从此切出 `feat/acp-runtime`）  
> **设计基线:** [`docs/acp-runtime-plan.md`](./acp-runtime-plan.md) — **架构决策已定，禁止重开设计**，只实现。  
> **验证锚点:** `acp:opencode`（`opencode acp`）。Phase 门禁与产品验收以设计稿 **§12（功能 / 显示 / 输入区）** 为准；不得用「mock 绿了」代替 OpenCode UI 验收。

---

## 0. 怎么用这份计划（给编排者 / 用户）

### 0.1 一句话

用 **Paseo / Claude Code 多 agent** 开三条常驻角色：

| 角色 | 职责 | 是否改代码 |
| --- | --- | --- |
| **Dev** | 按 phase 实现/修 bug；**可用 subagent 加速**；完成后 **直接 send Test** | 是 |
| **Test** | **按实际 diff 自定多角度测试**（文档/Dev 建议仅参考）；可用 subagent；完成后 **直接 send Dev**；只提缺陷不改业务 | 仅测码/fixture |
| **Watcher** | 每 20m 巡检：卡死救援、补发遗忘的通知、推进 phase、goal 结案 | 只写状态/日志 |

**主协作环：Dev ↔ Test 直连。** Watcher 是安全网，不是每条消息的中转。  
编排者启动一次（灌 `docs/acp-prompts/*.md` + Watcher 20m），直到 `goal_met=true`。

### 0.2 已登记三角色（2026-08-17）与 Prompt 文件

| 角色 | title | agentId | 建议 prompt 文件 |
| --- | --- | --- | --- |
| Dev | 开发 | `ad172813-30c1-4bea-82fe-ddfa1e4b0885` | [`docs/acp-prompts/dev.md`](./acp-prompts/dev.md) |
| Test | 测试 | `d62ccd50-e114-4625-a058-6bb75a76dad6` | [`docs/acp-prompts/test.md`](./acp-prompts/test.md) |
| Watcher | 监工 | `ed60735f-697e-4222-b470-69557ae6d9e3` | [`docs/acp-prompts/watcher.md`](./acp-prompts/watcher.md) + 每 20 分 [`watcher-loop-tick.md`](./acp-prompts/watcher-loop-tick.md) |

worktree：`/home/hachi/.paseo/worktrees/293djwjk/precious-husky`（`wks_d4c2675b95c38ad9`）。

```bash
paseo send ad172813-30c1-4bea-82fe-ddfa1e4b0885 --prompt-file docs/acp-prompts/dev.md --no-wait
paseo send d62ccd50-e114-4625-a058-6bb75a76dad6 --prompt-file docs/acp-prompts/test.md --no-wait
paseo send ed60735f-697e-4222-b470-69557ae6d9e3 --prompt-file docs/acp-prompts/watcher.md --no-wait
```

**Watcher 20 分钟：** 在监工会话用 heartbeat / `/paseo-loop`（见 `docs/acp-prompts/README.md`）。注意本机 Paseo CLI 0.4.0 可能无 `paseo loop` 子命令——以给**同一**监工 agent 循环 `send` tick 为准，不要每 20 分钟新建写代码的 worker。

§3.1–3.3 的 bootstrap 若与 `docs/acp-prompts/*.md` 冲突，**以 acp-prompts 为准**。

### 0.2.1 协作补充（强制）

1. **Subagent：** Dev、Test 工作中均可拉 subagent 并行；创建者对合入/结论负责。  
2. **测试独立：** Test 根据 **git diff / 交付行为** 制定多角度计划；禁止只执行设计文档清单或只测 Dev 点名项；§12 是覆盖率灵感与发布目标，不是逐步脚本。  
3. **直连通知：**  
   - Dev：`phase_gate=dev_done` 并写 log 后 → 立刻 `paseo send` Test  
   - Test：`test_passed` / `test_failed` 并写 defects/log 后 → 立刻 `paseo send` Dev  
4. **Watcher：** 20 分钟 tick 仅救援（忘 send、锁超时、idle 过久、phase 出口、goal_met）；已有正确 running 时不重复轰炸。

### 0.3 绝对约束（所有角色共用）

1. **设计已定**：双轨 `pty | acp`；不迁移现有 claude/codex/dsh/pi；不加 managed agent home；不改用户全局 CLI 配置。  
2. **脏工作区**：保留无关改动（当前已知 `ui/components/layout/LeftSidebar.tsx` 有用户改动 — **禁止 revert/覆盖**）。  
3. **Cargo** 必须在 `src-tauri/` 或 `--manifest-path src-tauri/Cargo.toml`。  
4. **UI 自动化受限**（Wayland）：不以截图/注入键盘为验收主路径；以后端 mock 测试 + `cargo test` + `pnpm tsc --noEmit` + 代码走读为主。  
5. **不问用户决策题**：灰色地带选「更安全的 MVP」（permission 默认 ask、fs write 默认关、未知 update 忽略）。  
6. **提交策略**：每完成一个 phase gate 由 Dev 提交一次（中文/英文均可，message 含 phase 号）；**不 force-push main**；不改签名密钥。  
7. **结束必须留中文交接** 在 `docs/acp-dev-status.md`（见 §1）。  
8. **Dev↔Test 完成必互 notify**；不得只改 status 不 send。

---

## 1. 共享状态（持久化，跨 agent 唯一真相）

所有角色**只通过文件协调**，不依赖聊天记忆。

### 1.1 文件布局

```text
docs/
  acp-runtime-plan.md          # 设计基线（只读，可回写附录 A）
  acp-multi-agent-dev-plan.md  # 本编排（只读，除非 Watcher 修订流程）
  acp-dev-status.md            # 【可写】当前 phase、锁、缺陷、交接
  acp-dev-log/                 # 【可写】滚动日志
    YYYYMMDD-HHMM-dev.md
    YYYYMMDD-HHMM-test.md
    YYYYMMDD-HHMM-watcher.md
src-tauri/tests/fixtures/      # mock ACP agent 等
```

### 1.2 `docs/acp-dev-status.md` 模板（启动时由 Watcher 创建）

```markdown
# ACP Dev Status

updated_at: <ISO8601>
goal_met: false
current_phase: 0          # 0..5；完成 Phase N 的出口条件后 +1
phase_gate: pending       # pending | dev_done | test_running | test_failed | test_passed | blocked
owner_lock: none          # none | dev | test | watcher
blocked_reason: ""

## Definition of Done（总目标，对应设计 §11）
- [ ] 边际成本：新 ACP agent = 一条 descriptor，零新 Rust adapter
- [ ] 双轨无损：既有 PTY `cargo test` 全绿
- [ ] 闭环：spawn → prompt → 流式 chunk → permission → cancel → kill（mock 自动化覆盖）
- [ ] 安全：默认不写盘、出界读拒绝、permission 默认 ask
- [ ] `pnpm tsc --noEmit` 通过
- [ ] 文档：附录 A 有实测 launch；ai-runtime-references / RUNBOOK 有链接

## Phase checklist
### Phase 0 — 协议摸底
- [ ] 选定 mock 或真实 agent
- [ ] 附录 A 填写
- [ ] 最小 initialize→session/new→prompt 日志

### Phase 1 — 后端 Host MVP
- [ ] 依赖 + acp/ 模块
- [ ] bridge/host/events/descriptor/registry
- [ ] lib.rs 分叉 + commands
- [ ] mock 测试绿

### Phase 2 — 前端面板 MVP
- [ ] runtimeTransport + store/actions
- [ ] AcpSessionPanel + ContentArea
- [ ] Composer 分发 + cancel
- [ ] tsc 绿

### Phase 3 — 权限与安全
- [ ] permission 闭环 + 卡 UI
- [ ] fs/read 沙箱
- [ ] security-review 补丁段落

### Phase 4 — 产品化
- [ ] Settings CRUD
- [ ] resume_key / session/load
- [ ] usage → 上下文条
- [ ] 文档

### Phase 5 — 增强（非阻断总目标；goal_met 可不包含）
- [ ] 按需

## Open defects（Test 写入，Dev 认领）
| id | phase | severity | title | repro | status |
| --- | --- | --- | --- | --- | --- |
| （空） |  |  |  |  |  |

## Last handoff（中文，每次 Watcher tick 更新摘要）
-

## Agent heartbeats
| agent | last_seen | state | note |
| --- | --- | --- | --- |
| dev |  |  |  |
| test |  |  |  |
| watcher |  |  |  |
```

### 1.3 锁协议

- 改业务代码前：Dev 写 `owner_lock: dev`；做完写回 `none` 并 `phase_gate: dev_done`。  
- 跑测试/改 fixture 前：Test 写 `owner_lock: test`；结束写 `test_passed` 或 `test_failed` + defects。  
- **禁止** 两边同时改同一文件；若冲突，Watcher 判 Dev 优先业务、Test 优先 `tests/` 与 fixtures。  
- 任一角色见 `owner_lock` 非己且 `< 30min`：等待或做只读工作。  
- `owner_lock` 超时（>45min 无 heartbeat）：Watcher 强制 `none` 并记 log「抢锁救援」。

---

## 2. Phase 与出口（机器可判定）

每个 phase **出口条件**必须可被 Test 用命令验证。Watcher 仅在 `test_passed` 时递增 `current_phase`。

| Phase | Dev 交付物 | Test 硬门槛（全过才 pass） |
| --- | --- | --- |
| **0** | 附录 A + `docs/acp-dev-log/*phase0*` 含一轮 NDJSON 往返记录；优先 **mock agent 脚本**（不依赖本机 gemini/goose 安装） | 脚本可独立运行：stdin 喂 initialize/session/new/prompt，stdout 有合法 NDJSON 响应 |
| **1** | `agent_runtime/acp/*`、`AcpBridge`、commands、list/spawn/kill 分叉、mock 集成测试 | `cd src-tauri && cargo test acp -- --nocapture` 相关测例绿；**全量** `cargo test` 绿（PTY 无回归） |
| **2** | 前端 transport 分叉、AcpSessionPanel、Composer、事件订阅 | `pnpm tsc --noEmit`；抽查无 `runtime === "gemini"` 硬编码；ACP 路径不调用 `agent_write`（grep 守卫） |
| **3** | permission 状态机 + UI 卡、fs/read 沙箱、mode=ask | cargo 测：permission deny；fs 出界 error；security 段落已写 |
| **4** | Settings CRUD、resume、usage、文档链接 | CRUD 单测或描述符读写测；文档文件存在链接；全量 cargo + tsc |
| **5** | 可选，不挡 `goal_met` | — |

**总目标 `goal_met=true` 条件（Watcher 判定）：**

```text
current_phase >= 4
AND phase_gate == test_passed
AND Definition of Done 全部勾选
AND 全量 cargo test 最近一次 log 为绿
AND pnpm tsc --noEmit 最近一次为绿
AND Open defects 无 severity=blocker 且 status!=closed
```

---

## 3. 三角色详细剧本

### 3.1 Dev Agent

**身份：** 唯一业务实现者。  
**输入：** `acp-runtime-plan.md` + `acp-dev-status.md` 的 `current_phase` 与 Open defects。  
**输出：** 代码、phase 内 commit、更新 status checklist、写 `acp-dev-log/`。

#### Bootstrap prompt（创建时粘贴）

```text
你是 CaPilot ACP Runtime 的 Dev Agent。在当前 git worktree 工作。

只读设计：docs/acp-runtime-plan.md
编排与状态：docs/acp-multi-agent-dev-plan.md 、 docs/acp-dev-status.md

规则：
1. 架构已定，禁止重新设计；禁止迁移现有 PTY runtime。
2. 每次醒来：读 acp-dev-status.md → 若 goal_met 则写中文交接并停。
3. 若 phase_gate 是 test_failed：优先修 Open defects（按 severity）。
4. 若 phase_gate 是 pending/test_passed 且轮到实现：认领 owner_lock=dev，做 current_phase 的最小可合并切片，跑你改动相关的 cargo test / tsc，提交，勾 checklist，设 phase_gate=dev_done，owner_lock=none。
5. 若 owner_lock 是他人且未超时：只做调研笔记，不改代码。
6. 保留工作区无关用户改动（尤其 LeftSidebar.tsx）。
7. 不要问用户选择题；默认更安全的 MVP。
8. 每轮结束更新 docs/acp-dev-status.md 的 Last handoff（中文）和 heartbeat。
9. 禁止 pnpm tauri dev 长时间占坑；验证用单元测试。需要真实 agent 时优先 tests/fixtures mock。
10. is_acp_runtime 必须在 get_adapter 之前分叉，严禁 acp:* 掉进 ClaudeAdapter。

Phase 切片提示（严格按 status.current_phase；锚点 acp:opencode）：
- 0: 本机 opencode acp 摸底 + 附录 A；可附 mock fixture；不必接 UI
- 1: Cargo 依赖 agent-client-protocol；acp/*；默认 descriptor→acp:opencode；lib.rs 分叉；mock+OpenCode 无 UI prompt
- 2: runtimeTransport（禁止 ===opencode 判 ACP）；AcpSessionPanel；Composer acp_prompt；藏 PTY OpenCode 方言
- 3: permission UI+后端；OpenCode 真工具；fs read 沙箱；security-review
- 4: Settings CRUD；resume；usage；§12 总表；RUNBOOK + ai-runtime-references

现在执行：读状态 → 做一轮最大 1 个 phase 切片或修 bug → 更新状态 → 停（等 Watcher/调度再次唤醒）。
```

#### Dev 每轮工作流（强制顺序）

```text
1. Read docs/acp-dev-status.md
2. if goal_met → 中文交接 → exit
3. if blocked_reason 非空且非己能解 → 记 log → exit
4. Claim lock
5. git status（勿碰无关文件）
6. 实现 / 修缺陷（单 phase，避免一次跨 2 个 phase）
7. 本地验证：
   - 后端：cd src-tauri && cargo test <filter>
   - 前端：pnpm tsc --noEmit（若动了 ui/）
8. git add 相关文件 → commit（message: "feat(acp): phase N — …"）
9. 更新 status：checklist、phase_gate=dev_done、heartbeat、handoff
10. Release lock
```

#### Dev 禁止事项

- 不开 Phase 5 除非 0–4 全绿且无事可做  
- 不改 `pty_core.rs` / 各 PTY runtime 业务逻辑（除非修 ACP 分叉误伤）  
- 不添加 `runtime === "xxx"` 新硬编码；只认 `transport` / `acp:` 前缀  
- 不把 ACP 画进 xterm  

---

### 3.2 Test Agent

**身份：** 质量闸门；可写 **仅** `src-tauri/tests/**`、`**/fixtures/**`、测码旁 `#[cfg(test)]` 增强；**不改**产品业务逻辑（发现业务 bug 只开 defect）。  
**输入：** `phase_gate=dev_done` 或定期回归。  
**输出：** 测试结果 log、Open defects 表、`phase_gate=test_passed|test_failed`。

#### Bootstrap prompt

```text
你是 CaPilot ACP Runtime 的 Test Agent。

设计：docs/acp-runtime-plan.md §8
状态：docs/acp-dev-status.md
编排：docs/acp-multi-agent-dev-plan.md

规则：
1. 每次醒来读 status。goal_met → 停。
2. 仅当 phase_gate 为 dev_done，或 watcher 标注 retest，或距上次全量回归 >2h：执行测试。
3. owner_lock=test；只改测试与 fixture。
4. 按 current_phase 执行对应门槛（见编排 §2），并且：
   - 永远跑：cd src-tauri && cargo test  （全量，防 PTY 回归）
   - 若 ui/ 有变更或 phase>=2：pnpm tsc --noEmit
5. 失败 → Open defects 增行（id=DEF-00x，severity，repro 命令，status=open），phase_gate=test_failed。
6. 全过 → 勾 DoD 能勾的项，phase_gate=test_passed，defect 可关则 closed。
7. 在 docs/acp-dev-log/ 写本次命令与摘要；更新 heartbeat；中文 handoff。
8. 不要实现功能来“让测试通过”；那是 Dev 的事。
9. Mock agent 必须可重复、无网络。

现在执行一轮。
```

#### 标准测试命令清单

```bash
# 始终
cd src-tauri && cargo test 2>&1 | tee /tmp/acp-cargo-test.log
# Phase 1+ 过滤（额外）
cd src-tauri && cargo test acp 2>&1 | tee /tmp/acp-cargo-acp.log

# Phase 2+
pnpm tsc --noEmit 2>&1 | tee /tmp/acp-tsc.log

# 静态守卫（Phase 2+）
rg -n 'agent_write' ui/state ui/components/acp ui/components/layout/Composer.tsx || true
rg -n 'isAcpRuntime|transport' ui/state ui/components --glob '*.ts*' | head
rg -n 'get_adapter\(|acp:' src-tauri/src/lib.rs src-tauri/src/agent_runtime

# Mock 脚本烟雾（Phase 0+）
python3 src-tauri/tests/fixtures/mock_acp_agent.py --self-test   # 若实现了
```

#### Defect 格式（强制）

```markdown
| DEF-003 | 1 | blocker | prompt 后无 message_chunk | `cargo test acp_bridge_prompt -- --nocapture` | open |
```

severity：`blocker`（挡 gate）/ `major` / `minor`。

---

### 3.3 Watcher Agent

**身份：** 调度与救援；**默认不改业务代码**。可写 status、log、必要时拆分 task 文件；可 `send_agent_prompt` 唤醒 Dev/Test。  
**输入：** 三方 heartbeat、gate、git 状态。  
**输出：** 推进 phase、清锁、唤醒、最终 `goal_met`、中文总交接。

#### Bootstrap prompt

```text
你是 CaPilot ACP Runtime 的 Watcher（调度）。

状态文件：docs/acp-dev-status.md（你维护 goal_met / current_phase / phase_gate / lock 救援）
编排：docs/acp-multi-agent-dev-plan.md
设计：docs/acp-runtime-plan.md（只读）

每轮（被 heartbeat 或手动唤醒时）：
1. 读 status + 最近 acp-dev-log + git status/log -5
2. 更新自己的 heartbeat
3. 判定：
   a. goal_met 已 true → 确认 DoD，写最终中文交接，停止调度
   b. owner_lock 超时 >45min → 释放 lock，记「救援」
   c. phase_gate=dev_done 且 test 空闲 → send prompt 给 Test：「重测 phase N」
   d. phase_gate=test_failed → send prompt 给 Dev：「修 DEF-xxx」
   e. phase_gate=test_passed 且 current_phase 未完成 DoD 勾选 → 你勾选可自动勾的项；若 phase 出口满足 → current_phase += 1，phase_gate=pending，唤醒 Dev
   f. phase_gate=pending 且 dev 空闲 → 唤醒 Dev 做 current_phase
   g. 两 agent 均 >30min 无 heartbeat → 尝试 send_agent_prompt 重启；仍无则 blocked_reason=agents_dead，写交接求用户
4. 不要自己实现 Phase 功能（除非 Dev/Test 双死且用户授权；默认不）
5. 每次写简短中文 Last handoff：现在 phase、谁在干、下一步、风险
6. 当 §2 总目标条件满足：goal_met=true，phase_gate=test_passed，写「用户醒来后建议手工点验」清单

可用 MCP：list_agents、get_agent_status、send_agent_prompt、get_agent_activity。
现在执行一轮 tick。
```

#### Heartbeat 建议

- Watcher：cron/heartbeat **每 15 分钟**  
- Dev/Test：无固定 cron；由 Watcher `send_agent_prompt` 事件驱动；也可各自 20 分钟 idle 自检  

#### 卡死模式识别

| 症状 | 动作 |
| --- | --- |
| Dev 连续 3 轮同一 DEF 未关 | Watcher 标注 blocked，建议缩小 scope / 降级自研 NDJSON（设计 §9） |
| cargo 编译 agent-client-protocol 失败 | 允许 Dev 按设计降级：schema 类型 + 自研 NDJSON（记入 status） |
| 本机无真实 ACP CLI | Phase 0/全程以 mock 为准；附录 A 写 mock；真实 agent 标「用户环境待验」 |
| tsc 与业务无关的既有错误 | 记录；不得要求 Dev 修全仓历史债，除非本次引入 |
| 测试 flaky | Test 先稳定 fixture；标 major 不标 blocker 除非必现 |

---

## 4. 实现顺序细化（Dev 的任务卡）

每张卡 = 一次 commit 粒度。Watcher 只按 phase 推进；Dev 卡内自左至右。

### Phase 0 卡

| ID | 任务 | 验收 |
| --- | --- | --- |
| P0-1 | `src-tauri/tests/fixtures/mock_acp_agent.py`：NDJSON；initialize / session/new / session/prompt / 推 chunks / 可选 request_permission / cancel | `--self-test` 或 printf 管道成功 |
| P0-2 | 回写 `acp-runtime-plan.md` 附录 A（mock 行 + 若有真实 CLI） | 文档有可复制 command/args |
| P0-3 | 记录样例日志到 `docs/acp-dev-log/` | 文件存在 |

### Phase 1 卡

| ID | 任务 | 验收 |
| --- | --- | --- |
| P1-1 | `Cargo.toml` + `acp/mod.rs` 骨架、`is_acp_runtime` | compile |
| P1-2 | `descriptor.rs` 读 `~/CaPilot/acp-agents.json`（缺省空列表） | 单测 |
| P1-3 | `host.rs` spawn 管道 + initialize + session/new | mock 测 |
| P1-4 | `bridge.rs` 会话表 + prompt/cancel/kill/status | mock 测 |
| P1-5 | `events.rs` + emit（测试用 Vec sink 亦可） | 收到 message_chunk |
| P1-6 | `registry.rs` + `RuntimeInfo.transport` | list 含 acp:* |
| P1-7 | `lib.rs`：AppState、spawn/kill/list 分叉、`acp_prompt`/`acp_cancel`；**禁止** acp 走 get_adapter | 全量 cargo test |
| P1-8 | `agent_write` 对 acp id 明确报错 | 单测 |

### Phase 2 卡

| ID | 任务 | 验收 |
| --- | --- | --- |
| P2-1 | `ui/state/runtimeTransport.ts` | 单测或纯函数 |
| P2-2 | store：acp 消息/状态结构 | tsc |
| P2-3 | `agentActions`：spawn 后订阅、send 分发 | tsc |
| P2-4 | `AcpSessionPanel.tsx` MVP | tsc |
| P2-5 | `ContentArea` 分叉 | tsc |
| P2-6 | `Composer` 隐藏 PTY 控件 + cancel | tsc + grep 守卫 |

### Phase 3 卡

| ID | 任务 | 验收 |
| --- | --- | --- |
| P3-1 | `permission.rs` + `acp_respond_permission` | 测 deny |
| P3-2 | `AcpPermissionCard` + 面板接入 | tsc |
| P3-3 | `fs/read_text_file` cwd 沙箱 | 出界失败测 |
| P3-4 | `security-review.md` ACP 段 | 文档 |

### Phase 4 卡

| ID | 任务 | 验收 |
| --- | --- | --- |
| P4-1 | `acp_list/upsert/remove` + descriptor 写回 | 测 |
| P4-2 | Settings UI CRUD | tsc |
| P4-3 | resume_key + session/load 分叉 | 测（mock 支持 load） |
| P4-4 | usage_update → 前端上下文 | tsc |
| P4-5 | RUNBOOK + ai-runtime-references 链接 | 文档 |
| P4-6 | 全量 cargo + tsc 最终绿 | gate |

---

## 5. 通信与唤醒协议

### 5.1 Watcher → Dev

```text
[acp-watcher] phase_gate=pending current_phase=1。
请读 docs/acp-dev-status.md，认领 lock，完成 Phase 1 下一张未勾任务卡（P1-x），
本地 cargo test 后 commit，设 dev_done。不要跨 phase。
```

### 5.2 Watcher → Test

```text
[acp-watcher] phase_gate=dev_done current_phase=1。
请全量 cargo test + acp 过滤；按编排 §2 写 defects 或 test_passed。
```

### 5.3 Test → 状态（不直接命令 Dev）

只写 Open defects；Watcher 转唤醒 Dev。

### 5.4 人类插入

用户若中途留言：Watcher 写入 `blocked_reason` 或 `docs/acp-dev-status.md` 的 `## Human notes`，Dev 下轮优先读。

---

## 6. 风险管理（执行期）

| 风险 | 自动缓解 |
| --- | --- |
| crate API 不适合长会话 | Phase 1 前 2 卡验证；失败则自研 NDJSON + 官方类型，status 记录 ADR 一行 |
| 脏树 LeftSidebar | 所有 commit 明确路径；`git add` 禁止 `git add -A` 无脑加 |
| 依赖下载慢/失败 | 重试 2 次；仍失败 blocked_reason=network |
| 测试误伤 ENV | 沿用 `ENV_LOCK`；ACP 测勿乱改 HOME |
| 范围膨胀 | Watcher 拒 Phase 5 直到 goal_met |
| 权限静默放行 | 代码审：默认 ask；Test grep `yolo`/`allow_all` 在 MVP 路径 |

---

## 7. 最终交付物清单（goal_met 时必须存在）

- [ ] `src-tauri/src/agent_runtime/acp/` 模块完整  
- [ ] `src-tauri/tests/fixtures/mock_acp_agent.py`（或等效）  
- [ ] 前端 `AcpSessionPanel` + transport 分发  
- [ ] Settings ACP CRUD（Phase 4）  
- [ ] `docs/acp-runtime-plan.md` 附录 A 已填  
- [ ] `docs/ai-runtime-references.md`、`docs/CaPilot-IDE-RUNBOOK.md`、`docs/security-review.md` 已更新  
- [ ] `docs/acp-dev-status.md`：`goal_met: true` + 完整中文交接  
- [ ] git log 上可见 phase 递进 commits  

### 用户醒来后建议手工点验（Watcher 写入交接）

1. Settings 加一条指向 mock 或真实 CLI 的 descriptor  
2. 新 tab 选 `acp:…` → 发「ping」→ 见流式文本  
3. 触发 permission → 拒绝应不继续危险工具  
4. 取消 turn / 关 tab  
5. 开一个 **claude PTY** tab，确认与改前一致  

（Wayland 下若 UI 自动化不可用，以用户手点为准；自动化已由 mock+cargo 覆盖协议层。）

---

## 8. 编排者（主 session）最小启动清单

```text
[ ] 1. 确认分支与 worktree；保留 LeftSidebar 用户改动
[ ] 2. 写 docs/acp-dev-status.md（模板 §1.2），current_phase=0，gate=pending
[ ] 3. mkdir -p docs/acp-dev-log
[ ] 4. 创建 Dev / Test / Watcher 三 agent，initialPrompt = §3.x bootstrap
[ ] 5. 给 Watcher 设 heartbeat：每 15m「执行 Watcher tick」
[ ] 6. 立即 send Watcher 第一轮 tick（会拉起 Dev）
[ ] 7. 主 session 可退出；回来只看 docs/acp-dev-status.md
```

### 8.1 主 session 也可「单进程模拟三角色」（无 Paseo 时）

若不能开三进程，主 agent 严格轮转：

```text
loop:
  扮演 Watcher tick（只改 status）
  if need dev: 扮演 Dev 一轮
  if need test: 扮演 Test 一轮
  if goal_met: break
  sleep / schedule 15m
```

仍必须维护同一 status 文件，防止上下文丢失后发散。

---

## 9. 成功标准（与设计 §11 对齐，可机器勾选）

1. 新 ACP agent：**仅** descriptor，无新 `runtimes/*.rs`  
2. 既有 PTY：`cargo test` 全绿  
3. Mock 闭环：start → prompt → chunks → permission → cancel → kill  
4. 安全默认：read 沙箱、write off、permission ask  
5. `pnpm tsc --noEmit` 绿  
6. 中文交接完整，用户可按 §7 手点  

---

*本编排与 `acp-runtime-plan.md` 冲突时，**以设计稿的产品/架构决策为准**；本文件只约束「怎么无人值守做完」。*
