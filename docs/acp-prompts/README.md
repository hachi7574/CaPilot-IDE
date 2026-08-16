# 发给三个 Paseo agent 的 Prompt

> 更新：2026-08-17（Dev↔Test 直连 + subagent + 测试独立多角度）  
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
2. **测试独立：** 按**实际 diff/交付**自定多角度计划；设计文档与 Dev 建议只是参考，禁止只测文档清单或只测 Dev 点名项。  
3. **主通知路径：** Dev 完成 → **直接 send Test**；Test 完成 → **直接 send Dev**。Watcher 20 分钟只做卡死救援与 phase/goal 推进。  

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
