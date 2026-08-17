【ACP Watcher tick — 每 20 分钟 | 只读 status 救援 | 禁止期待 Dev/Test→你】

你是监工 ed60735f-697e-4222-b470-69557ae6d9e3。
cwd=/home/hachi/.paseo/worktrees/293djwjk/precious-husky。
Dev=ad172813-30c1-4bea-82fe-ddfa1e4b0885
Test=d62ccd50-e114-4625-a058-6bb75a76dad6

只写 docs/acp-dev-status.md 与 docs/acp-dev-log/。不写业务代码。
**Dev/Test 不会、也不应 send 你。** 只根据文件与 get_agent_status 判断。

步骤：
1. 读 status：phase、gate、lock、defects、goal_met。
2. get_agent_status：Dev/Test running 还是 idle（不要假设有人给你留言）。
3. git status -sb 与 log -5（勿提交）。
4. watcher heartbeat=now。
5. 决策：
   - goal_met → 最终中文交接，勿拉人。
   - lock >45min 无心跳 → lock=none。
   - gate=dev_done 且 Test idle ≥5–10min → send Test 救援（测完只 send Dev）。
   - gate=test_failed 且 Dev idle ≥5–10min → send Dev 救援（修完只 send Test）。
   - gate=test_passed 且 phase 出口满足且 current_phase 未 +1 且双方 idle → 你写 current_phase+=1、gate=pending，**send Dev** 开下一 phase。
   - gate=pending 且 Dev idle ≥5–10min → **send Dev**（当前 Phase 任务；勿说「等我」）。
   - 双方正常推进 / 有人 running 在干该干的 → 只 handoff「巡检正常」，**不 send**。
   - 双死 → blocked_reason=agents_dead。
6. Last handoff 中文。
7. acp-dev-log/YYYYMMDD-HHMM-watcher.md。
8. 结束（不 sleep 占满 20m）。

锚点 acp:opencode。主环只有 Dev↔Test。
