# Context Window Usage

Paseo displays the provider's latest estimate of the active model context. It does not tokenize the timeline or derive context size in the app.

Keep context-window usage separate from provider plan usage. Context-window usage is session state pushed by an agent adapter. Plan usage is account quota fetched on demand; see [providers.md](providers.md#provider-usage-fetchers).

## Contract

Provider adapters normalize context data into the optional `AgentUsage` fields defined in `packages/protocol/src/agent-types.ts`:

- `contextWindowUsedTokens` is the provider's estimate of tokens currently occupying the model context.
- `contextWindowMaxTokens` is the selected model's context capacity.

These fields are not cumulative token spend. Compaction can reduce `contextWindowUsedTokens`, while cumulative input, output, and cost continue to increase. Do not reconstruct the value from timeline rows: hidden prompts, tool payloads, cache accounting, provider-side compaction, and provider-owned history make that estimate incomplete.

Both fields stay optional. A provider that cannot supply a trustworthy current value should omit it instead of estimating it from visible text.

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

### Codex

Use the `last` usage object, not the session-total object. Codex's `last.total_tokens` represents the current context reading needed by the meter; cumulative totals answer a different question.

### OpenCode

Use the model attached to the assistant message when available. It can differ from the draft selection, so observed assistant-model metadata updates the maximum. `step-finish` is the accounting boundary for used context; session cost is accumulated separately.

### Pi and OMP

Treat runtime session statistics as authoritative. Pi refreshes after completion. OMP polls because its runtime exposes the current context independently of stream events. Publish only changed OMP samples to avoid agent-state churn.

## Adding or changing a provider

Before exposing a context meter:

1. Identify an upstream value for current active context, not lifetime token consumption.
2. Identify the effective selected model's capacity. Do not assume every model under one provider has the same window.
3. Emit `usage_updated` when a meaningful live sample changes, and include final usage when the completion API supplies it.
4. Preserve compaction semantics: the used value must be allowed to decrease.
5. Omit unsupported fields. The UI already handles partial and unavailable data.
6. Test provider normalization, `AgentManager.lastUsage` reconciliation, wire projection, and the missing/pending/renderable meter states affected by the change.

Do not add a second context-usage store or a client-side tokenizer. `AgentUsage` and `lastUsage` are the shared contract across provider, daemon, protocol, and app layers.
