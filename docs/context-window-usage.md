# Context Window Usage

Paseo displays the provider's latest estimate of the active model context. It does not tokenize the timeline or derive context size in the app.

Keep context-window usage separate from provider plan usage. Context-window usage is session state pushed by an agent adapter. Plan usage is account quota fetched on demand; see [providers.md](providers.md#provider-usage-fetchers).

## Contract

Provider adapters normalize context data into the optional `AgentUsage` fields defined in `packages/protocol/src/agent-types.ts`:

- `contextWindowUsedTokens` is the provider's estimate of tokens currently occupying the model context.
- `contextWindowMaxTokens` is the selected model's context capacity.

These fields are not cumulative token spend. Compaction can reduce `contextWindowUsedTokens`, while cumulative input, output, and cost continue to increase. Do not reconstruct the value from timeline rows: hidden prompts, tool payloads, cache accounting, provider-side compaction, and provider-owned history make that estimate incomplete.

Both fields stay optional. A provider that cannot supply a trustworthy current value should omit it instead of estimating it from visible text.

### Cache hit rate

`AgentUsage` also carries two **session-cumulative** prompt-token counts: `cacheHitTokens` (the cached-read portion) and `cacheTotalInputTokens` (the total prompt). The composer renders the ratio `cacheHitTokens / cacheTotalInputTokens` as a small chip next to the meter; both must be present and the denominator must be positive, otherwise nothing renders.

A measured zero cache hit is reported as `cacheHitTokens: 0`, not `null`, so the composer renders 0%. `actualModel` is the last provider-observed model id. It is display-only: the persisted/configured model remains authoritative for spawning, switching, and catalog checkmarks.

The two counts are NOT comparable across providers, and each adapter normalizes its runtime's accounting before reporting them:

| Runtime | Hit numerator | Total-prompt denominator |
| --- | --- | --- |
| Claude | `cache_read_input_tokens` | `input_tokens + cache_creation_input_tokens + cache_read_input_tokens` (input excludes cache reads) |
| Codex | `cached_input_tokens` (older transcripts: `cache_read_input_tokens`) | `input_tokens` (input **already includes** the cached portion — verified `total_tokens == input_tokens + output_tokens`) |
| OpenCode | `tokens.cache.read` | `tokens.input + tokens.cache.read + tokens.cache.write` (input excludes cache reads) |

Both are summed across the whole session transcript (Claude: unique assistant `message.id` records skipping `isSidechain`; Codex: all `token_count` events; OpenCode: all `step-finish` parts). The frontend only computes the percentage; it never cross-converts providers.

## Data flow

1. The provider adapter reads usage from its SDK, RPC runtime, model metadata, or stream events.
2. The adapter emits `usage_updated` during a turn or attaches usage to `turn_completed`.
3. `AgentManager` stores the value in `agent.lastUsage` and emits agent state. A streaming update replaces `lastUsage`; completion usage merges into the latest value so a completion that omits context fields does not erase a live reading.
4. The wire projection validates finite numbers and includes `lastUsage` in agent snapshots and `agent_update` messages.
5. The app reconciles the snapshot into the session store. The composer reads the two context fields from `agent.lastUsage`.

`lastUsage` lives only in daemon memory. A daemon restart clears it and the meter stays empty until the next turn rebuilds it; an agent reload (model switch or reconnection) preserves it.

The app renders a determinate meter only when both values are present. While an agent is initializing or running, missing data reserves the meter footprint with a loading ring. Missing data on an idle agent renders no meter.

The static capacity shown in the model picker (`contextWindowMaxTokens` on model definitions) is not a fallback for the meter. The meter is driven entirely by `lastUsage`.

The meter computes `used / max * 100`. It clamps the ring geometry to 100%, uses the warning color from 70%, and uses the destructive color above 90%. The tooltip keeps the provider values and shows the rounded percentage plus formatted used/max token counts.

Relevant boundaries:

- Provider normalization: `packages/server/src/server/agent/providers/`
- State reconciliation: `packages/server/src/server/agent/agent-manager.ts`
- Wire projection: `packages/server/src/server/agent/agent-projections.ts`
- App selection: `packages/app/src/composer/index.tsx`
- Rendering: `packages/app/src/components/context-window-meter.tsx`

## Provider sources

| Provider | Used tokens | Maximum tokens | Freshness |
| --- | --- | --- | --- |
| Claude | Current streamed request input plus output; result usage is the fallback. A compact boundary replaces it with post-compaction tokens. | Model manifest initially, then the largest valid `modelUsage.contextWindow` reported by the SDK. | During streaming, at completion, and after compaction. |
| Codex | `tokenUsage.last.total_tokens` from the app-server notification. | `tokenUsage.model_context_window`. | On token-usage notifications. |
| OpenCode | The latest `step-finish` input, output, reasoning, cache-read, and cache-write token total. | Selected or observed assistant model's `limit.context`. | At each `step-finish`. |
| Pi | `getSessionStats().contextUsage.tokens`. | `getSessionStats().contextUsage.contextWindow`. | After a turn completes. |
| OMP | `getSessionStats().contextUsage.tokens`. | `getSessionStats().contextUsage.contextWindow`; older runtimes may expose the same values through `get_state`. | Every three seconds during a turn and once at completion. |
| ACP adapters | Not mapped by the shared ACP adapter. | Not mapped by the shared ACP adapter. | No context meter until the adapter gains a trustworthy contract. |

Provider values do not have identical accounting rules. Preserve the upstream meaning instead of forcing input/output/cache fields into a cross-provider formula. The normalized fields promise displayable current occupancy and capacity, not token-accounting equivalence between providers.

### Claude

Claude needs a stateful fallback chain because its live stream, result message, and compaction events expose different pieces of the same fact. During streaming, use the current request's input and output. At result time, prefer that live value; otherwise use the active usage iteration, including cache creation and cache read tokens. On the first turn, a result message without iterations falls back to its legacy flat usage object. After compaction, use `postTokens` so the meter falls to the new active-context size.

The static model manifest supplies an initial maximum so live usage can render before SDK model usage arrives. Runtime model usage supersedes it because gateways and model variants can report a different effective window.

Resolve Claude's JSONL by the Agent's persisted `resume_key`, never by the newest file under the cwd project directory. Multiple IDE and standalone Claude processes can share the same cwd. The per-agent SessionStart hook sidecar supplies the exact `session_id` when an older record or startup race lacks it; persist that recovered key before reading. Claude can append repeated streaming snapshots with the same assistant `message.id`, so cumulative cache totals keep only the latest non-zero snapshot for each id. The last usable assistant `message.model` populates display-only `actualModel`.

### Codex

Use the `last` usage object, not the session-total object. Codex's `last.total_tokens` represents the current context reading needed by the meter; cumulative totals answer a different question.

Resolve the transcript by the agent's persisted Codex `resume_key` (the provider session id), then verify its cwd. Never choose the newest transcript by cwd: multiple IDE agents and standalone Codex processes can legitimately share one project directory. Codex hook sidecars carry `session_id` so fresh sessions can be bound directly. For legacy/missed captures, recover by matching the UUIDv7 session timestamp to the persisted Agent creation time within five seconds, require the cwd to match, and persist the recovered key before reading usage.

### OpenCode

Use the model attached to the assistant message when available. It can differ from the draft selection, so observed assistant-model metadata updates the maximum. `step-finish` is the accounting boundary for used context; session cost is accumulated separately.

Resolve every OpenCode query by the Agent's persisted provider session id and verify the session directory matches the Agent cwd. Do not use `ORDER BY time_updated DESC` by cwd: another IDE Agent, standalone TUI, or subagent can become newer. The per-process status plugin binds once to the first root-session `sessionID` observed on the event bus and writes it to the Agent sidecar. For sessions created before this binding existed, recovery is allowed only when exactly one cwd-matching session was created from 2 seconds before through 30 seconds after the Agent row; ambiguity returns no usage.

Use the latest exact-session `step-finish.tokens.total` for current occupancy. For cumulative cache statistics, use OpenCode's exact-session aggregate columns `tokens_input`, `tokens_cache_read`, and `tokens_cache_write`; the denominator is their sum and the numerator is `tokens_cache_read`. This is faster and avoids double-counting repeated parts.

### Pi and OMP

Treat runtime session statistics as authoritative. Pi refreshes after completion. OMP polls because its runtime exposes the current context independently of stream events. Publish only changed OMP samples to avoid agent-state churn.

## Adding or changing a provider

### Mandatory provider invariants

These checks are required for every new provider adapter. They prevent the same class of cache/context/model bugs from recurring:

1. **Exact session identity:** usage reads require the persisted provider session id. A cwd, project path, file mtime, “latest session”, PID guess, or global model state is never a session identity.
2. **Collision-safe capture:** capture the provider id from a per-Agent hook/event/stream whenever possible and persist it. A legacy time-window recovery must validate cwd and return a value only for one unambiguous candidate.
3. **Fail closed:** a missing, stale, mismatched, or ambiguous id returns no usage. Never borrow another conversation to keep the UI populated.
4. **Authoritative accounting boundary:** document whether the provider exposes final messages, step-finish events, request snapshots, or aggregate columns. Deduplicate repeated records by their stable provider id, or prefer authoritative aggregate columns.
5. **Provider-specific denominator:** verify whether input already includes cache reads. Record the numerator and denominator formula using real provider data; do not reuse another provider's formula.
6. **Zero is data:** when total prompt is positive, a measured zero hit is `0`, not missing/null. Missing means the provider supplied no trustworthy accounting.
7. **Actual model is telemetry:** expose the provider-observed model for display, but keep the configured model authoritative for spawning, switching, persistence, and catalog checkmarks.
8. **Non-blocking reads:** database/filesystem/subprocess work runs off the async command thread and expensive catalog discovery is cached. Performance failure must not be indistinguishable from a valid 0%.
9. **Required regression fixtures:** test two sessions sharing one cwd, a newer unrelated session, missing/ambiguous identity, repeated usage records, a valid 0% sample, the provider's denominator semantics, and actual-model/configured-model separation.

Code review for a new provider is incomplete until its documentation names the identity source, accounting boundary, cache formula, observed-model source, and the tests covering the invariants above.

Before exposing a context meter:

1. Identify an upstream value for current active context, not lifetime token consumption.
2. Identify the effective selected model's capacity. Do not assume every model under one provider has the same window.
3. Emit `usage_updated` when a meaningful live sample changes, and include final usage when the completion API supplies it.
4. Preserve compaction semantics: the used value must be allowed to decrease.
5. Omit unsupported fields. The UI already handles partial and unavailable data.
6. Test provider normalization, `AgentManager.lastUsage` reconciliation, wire projection, and the missing/pending/renderable meter states affected by the change.

Do not add a second context-usage store or a client-side tokenizer. `AgentUsage` and `lastUsage` are the shared contract across provider, daemon, protocol, and app layers.
