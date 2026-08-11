# 剩余用量抓取机制（Rate-Limit Usage Fetching）

本文梳理 Orca 如何获取各 AI 平台 / CLI 的**剩余用量**（配额窗口），不涉及本地历史用量分析（`claude-usage/`、`codex-usage/`、`opencode-usage/` 是扫本地 JSONL/SQLite 统计 tokens/成本的目录）。

## 统一编排 `RateLimitService`（`src/main/rate-limits/service.ts`）

- **轮询**：默认 15 分钟一次（`DEFAULT_POLL_MS`），下限 30s；窗口聚焦/显示/恢复、手动刷新、账户切换时额外触发；唤醒后的自动刷新至少间隔 5 分钟（`MIN_REFETCH_MS`）。
- **去重/节流**：相同窗口值 30s 内跳过渲染推送（`LIVE_CLAUDE_INGEST_DEDUPE_MS`）；失败按 30s → 15min 指数退避，上限连续 8 次（`MAX_ACTIVE_FAILURE_STREAK`）；HTTP `Retry-After` 窗口内不再请求（`isRetryAfterActive`）。
- **状态模型**：`ProviderRateLimits`（`src/shared/rate-limit-types.ts`），各 provider 以 `session(5h)`、`weekly(7d)`、`[monthly(30d)]`、`[fableWeekly]`、`[buckets]` 描述，并带 `status/error/usageMetadata`（来源、失败类型、凭据来源、重试时间）。
- **非活跃账户**：Claude/Codex 托管账户在账户切换器打开时按需抓取，60s debounce（`INACTIVE_FETCH_DEBOUNCE_MS`）+ 2s 错峰（`INACTIVE_CODEX_PROBE_STAGGER_MS`）。

## 1. Claude（`claude-fetcher.ts` + `claude-pty.ts` + `statusline-script.ts`）

| 路径 | 机制 | 详情 |
|---|---|---|
| ① **OAuth HTTP**（首选） | `GET https://api.anthropic.com/api/oauth/usage`（`claude-fetcher.ts:46`） | 凭据顺序：macOS Keychain（scoped → legacy）→ `~/.claude/.credentials.json`。头：`Authorization: Bearer <token>`、`anthropic-beta: oauth-2025-04-20`、`User-Agent: claude-code/2.1.0`，10s 超时。产出 `five_hour → session(300)`、`seven_day → weekly(10080)`、fable 窗口。API key 不用（会在 OAuth 端点 401），由 PTY 兜底服务。 |
| ② **statusline 实时**（主打、零成本） | 注入托管脚本（`statusline-script.ts`），Claude Code ≥2.1.80 每轮把 `rate_limits` JSON pipe 给 statusLine 命令 | 脚本按 pane 节流 15s/次（`CLAUDE_STATUSLINE_MIN_POST_INTERVAL_SECONDS`），只 POST 含 `rate_limits` 的 payload 到本地钩子 `/statusline/claude`（`statusline-script.ts:155`）；服务端 `ingestLiveClaudeRateLimits`（`service.ts:1472`）按 `CLAUDE_CONFIG_DIR` 归因账户，30s 内同窗口去重。不消耗线上用量接口预算。 |
| ③ **PTY 兜底**（`claude-pty.ts`） | 拉起交互式 `claude`，等 2s 后键盘输入 `/usage\r` | 用正则解析 TUI：`current session`/weekly/`fable` 的 "62% used/left"（`claude-pty.ts:27-37`），25s 超时、100KB 缓冲、自动接受信任确认。 |

失败时还有**令牌修复链**：OAuth 401（stale token）→ 委托 CLI 刷新 Keychain → 重试 OAuth；或直接退回 PTY。managed 账户按需抓取 `fetchManagedAccountUsage`（`claude-fetcher.ts:1179`）：读托管 auth 文件 → 过期先 refresh 令牌 → OAuth 抓取（无 PTY 兜底）。

## 2. Codex（`codex-fetcher.ts`）

入口 `fetchCodexRateLimits`（`codex-fetcher.ts:1198`）：先 `probeCodexAuthPresence` 探测登录态 → WSL 走后端 HTTP → 否则 JSON-RPC → 依错型回退 PTY；全程用 `withCodexHomeProcessLock` 防并发刷新 auth.json。

| 机制 | 详情 |
|---|---|
| **后端 HTTP**（WSL 专用，`fetchViaBackend:535`） | `GET https://chatgpt.com/backend-api/wham/usage`，认证头取自 auth.json，校验 `plan_type`（状态栏显示 Plus 等订阅档），产出 `primary_window → session`、`secondary_window → weekly` + `rate_limit_reset_credits`。 |
| **JSON-RPC**（常规首选） | 拉起 `codex -s read-only -a untrusted app-server`，JSON-RPC over stdin/stdout，`initialize` 响应后发 `account/rateLimits/read`（`codex-fetcher.ts:777`），产出 `rateLimits.primary → session`、`.secondary → weekly` + `rateLimitResetCredits`。超时：常规 10s / 初始化 30s，WSL 25s / 40s。 |
| **PTY 兜底**（`fetchViaPty:954`） | 拉起 `codex -s`，发 `/status\r`（Enter 单独按键、350ms 间隔、失败 3s 重试、boot 2.5s 后 nudge），解析 TUI 里 "5 hour"/"1 week" 百分比（`codex-rate-limit-window-classification.ts`：session=300min / weekly=10080min），15s 超时。 |
| **session 补充**（`withBackendSessionWindow:585`） | RPC/PTY 结果缺 session 时补调一次后端 HTTP。 |
| **消耗重置券**（`consumeCodexRateLimitResetCredit:441`） | 用户主动动作：`POST https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume`，body `{redeem_request_id: 幂等键}`，30s 超时，返回 `reset/nothingToReset/noCredit/alreadyRedeemed`。 |

**非活跃 Codex 账户**（`service.ts:690`）：直接把 `managedHomePath` 传给 `fetchCodexRateLimits`，**禁用 PTY**（RPC-only，避免每账户起隐藏 PTY 击穿 Windows ConPTY）。

## 3. OpenCode Go（`opencode-go-usage-fetcher.ts`，纯网页抓取）

单一路径 + cookie jar：

1. 读用户粘贴的 opencode.ai `auth`/`__Host-auth` cookie（裸 token 自动包成 `auth=...`，`normalizeCookieInput:26`），过滤无关 cookie。
2. 建隔离 Electron `Session`（`createOpenCodeRequestSession`），避免 Windows 下手动 Cookie 头被拒。
3. **发现 workspace**：调 SST server-fn `GET https://opencode.ai/_server?id=<def3997…哈希>`（带 `X-Server-Id`/`X-Server-Instance`），正则抓出 `wrk_`/`wk_` 的 id；也支持设置里的 workspaceIdOverride。
4. **抓用量页**：逐个 `GET https://opencode.ai/workspace/<id>/go`，用 `opencode-go-page-scraper.ts` 解析 React Flight 序列化 JS 里的 `rollingUsage`/`weeklyUsage`/`monthlyUsage`（括号配平 + depth-1 字段校验，跳过 `null` 占位）。
5. 产出：`rolling → session(300)`、`weekly(10080)`、`monthly(43200)`。每请求 15s 超时，抓完清空 cookie jar。

## 4. Gemini（`gemini-usage-fetcher.ts`，唯一 per-model buckets）

- `POST https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`（`gemini-usage-fetcher.ts:19`），body `{project}`，头 `Authorization: Bearer <token>`，10s 超时。
- 凭据：Gemini CLI 的 auth.json / credentials，支持从 CLI bundle 提取并主动刷新 token（区别于 Kimi）。
- 产出：解析 `buckets[]`（`remainingFraction`/`resetTime`/`modelId`）→ 各模型 `RateLimitBucket[]`，再聚合出 5h/7d 汇总窗口（`gemini-bucket-formatting.ts`）。
- 仅当设置开启 "Gemini CLI OAuth" 才抓取（`service.ts:1604`）。

## 5. Kimi（`kimi-fetcher.ts`，只读复用 CLI 凭据）

- `GET https://api.kimi.com/coding/v1/usages`（`kimi-fetcher.ts:338`），头 `Authorization: Bearer <access_token>` + `Accept: application/json`。
- token 只读自 `<kimi home>/credentials/kimi-code.json`（15 分钟 TTL，CLI 负责刷新），**Orca 绝不自刷**——刷新会登出活会话（`kimi-fetcher.ts:297-302`）；过期就返回 `delegated-refresh-required`，等下次 CLI 刷新文件。
- 支持 WSL home 解析（`service.ts:273`）。

## 6. Grok（`grok-fetcher.ts`，两条 billing 格式）

| 格式 | 端点 | 产出 |
|---|---|---|
| **统一计费** | `GET https://cli-chat-proxy.grok.com/v1/billing`（`grok-fetcher.ts:21`） | `currentPeriod`/`monthlyLimit`/`used`/`onDemandCap` → weekly(10080) + monthly(43200) |
| **积分** | `GET …/billing?format=credits`（`grok-fetcher.ts:18`） | `creditUsagePercent`/`prepaidBalance` → 同上窗口；controller 按返回结构决定用哪条 |

- 认证：`xai-grok-cli` 头，token 只读自 `~/.grok/auth.json` 或 `GROK_HOME`（`grok-auth.ts`）；未登录则状态栏只显示"未配置"。10s 超时。

## 7. MiniMax（`minimax-fetcher.ts` + `minimax-request-context.ts`）

- `GET https://platform.minimax.io/v1/api/openplatform/coding_plan/remains`（`minimax-request-context.ts:3`），固定 `Origin`/`Referer` 指向 console，10s 超时。
- 认证：用户粘贴含 `_token` 的 Cookie 头 + `groupId`；请求走**双通道**——优先隔离 `Session` cookie jar（`orca-minimax-rate-limit-fetch` 分区），失败回退手动 Cookie 头（`minimax-fetcher.ts:165`）。只放行白名单敏感 cookie（`_token`/`_abck` 等）。
- 产出：`remains` 接口的 session/weekly 数据；cookie 落在文件系统，重启后仍能识别配置。

## 8. Antigravity（无独立抓取）

- 纯镜像：把 Gemini 结果拷贝一份、改 `provider: 'antigravity'`（`service.ts:1735`），因目前共享谷歌凭据。

## 一览表

| Provider | 方式 | 认证 | 端点 | 产出窗口 |
|---|---|---|---|---|
| Claude | OAuth HTTP / statusline 实时 / PTY `/usage` | OAuth bearer（Keychain / credentials.json） | `api.anthropic.com/api/oauth/usage` | session 5h、weekly 7d、fableWeekly |
| Codex | 后端 HTTP / JSON-RPC / PTY `/status` | auth.json token | `chatgpt.com/backend-api/wham/usage`、app-server RPC | session 5h、weekly 7d、resetCredits |
| OpenCode Go | 网页抓取（server-fn + 用量页） | `auth` cookie | `opencode.ai/_server`、`/workspace/<id>/go` | session 5h、weekly 7d、monthly 30d |
| Gemini | HTTP Quota（per-model） | OAuth（可自刷新） | `cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota` | 模型桶 + 5h/7d 汇总 |
| Kimi | HTTP `/usages`（只读、不自刷） | access_token（`kimi-code.json`） | `api.kimi.com/coding/v1/usages` | session / weekly |
| Grok | HTTP billing（credits / unified 双格式） | `xai-grok-cli` 头（auth.json） | `cli-chat-proxy.grok.com/v1/billing` | weekly 7d、monthly 30d |
| MiniMax | HTTP remains（cookie jar / 手动头） | `_token` cookie + groupId | `platform.minimax.io/…/coding_plan/remains` | session / weekly |
| Antigravity | Gemini 镜像 | — | — | = Gemini |

结论：**HTTP 直连**是主流（6/8），Claude/Codex 额外保留 **PTY 伪装 CLI** 兜底，Claude 独有 **statusline 搭车**实现零成本实时更新；DeepSeek、Cursor 等没有专用抓取器，属于上面某个 provider 的模型。