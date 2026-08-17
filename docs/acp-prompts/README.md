# 发给三个 Paseo agent 的 Prompt

> 更新：2026-08-17（禁止 Dev/Test→Watcher；**Dev→Test 只报事实**；Test 自定测法）  
> worktree：`/home/hachi/.paseo/worktrees/293djwjk/precious-husky`  
> 设计：`docs/acp-runtime-plan.md`（锚点 **`acp:opencode`**）  
> 状态：`docs/acp-dev-status.md`

| 角色 | title | agentId | prompt |
| --- | --- | --- | --- |
| Dev | 开发 | `ad172813-30c1-4bea-82fe-ddfa1e4b0885` | [`dev.md`](./dev.md) |
| Test | 测试 | `d62ccd50-e114-4625-a058-6bb75a76dad6` | [`test.md`](./test.md) |
| Watcher | 监工 | `ed60735f-697e-4222-b470-69557ae6d9e3` | [`watcher.md`](./watcher.md) + 每 20 分 [`watcher-loop-tick.md`](./watcher-loop-tick.md) |

## 协作要点（给人类）

1. **Dev / Test 可用 subagent** 并行加速；本人对结果负责。  
2. **测试独立：** Test 只根据 **diff/源码** 自定测法；**Dev→Test 只报事实**（commit/paths/behavior/self_check/limits），**禁止**「请测试…」派工。  
3. **通知拓扑（硬）：**  
   - 允许：`Dev → Test`、`Test → Dev`、`Watcher → Dev|Test`（仅救援）  
   - **禁止：`Dev → Watcher`、`Test → Watcher`**  
4. **phase 推进：** Test 在出口满足时自己 +phase 并 send Dev；Watcher 20m 只补洞。  
5. Dev **不要**等 Watcher；Test **不要**只读文档打勾。

## 发送

```bash
cd /home/hachi/.paseo/worktrees/293djwjk/precious-husky

paseo send ad172813-30c1-4bea-82fe-ddfa1e4b0885 --prompt-file docs/acp-prompts/dev.md --no-wait
paseo send d62ccd50-e114-4625-a058-6bb75a76dad6 --prompt-file docs/acp-prompts/test.md --no-wait
paseo send ed60735f-697e-4222-b470-69557ae6d9e3 --prompt-file docs/acp-prompts/watcher.md --no-wait
```

建议顺序：先 Dev + Test 灌设定，再 Watcher（装 20m 心跳并首轮 tick）。

## Watcher 20 分钟

在监工会话：`/paseo-loop` 每 20m 跑 `watcher-loop-tick.md`，或 heartbeat cron `7,27,47 * * * *`。  
退出：`goal_met: true`。不要每轮新建写代码的 worker。
