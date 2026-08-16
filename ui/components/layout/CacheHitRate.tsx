import { useStore } from "../../state/store";
import { supportsContextUsage } from "../../state/usageContext";
import { formatTokens } from "./ContextWindowMeter";

/**
 * Session-cumulative cache hit rate for the composer target line
 * (docs/ai-runtime-references.md §2 — per-runtime accounting differs; the
 * adapters normalize `cacheHitTokens`/`cacheTotalInputTokens` into the same
 * "hit / prompt" meaning).
 *
 * Renders only when BOTH cache fields are present and the prompt total is
 * nonzero — no loading state, no placeholder; the chip pops in with its data.
 * Runtimes without a `context_usage` implementation (bash) never render it.
 */
export function CacheHitRate({ agentId }: { agentId: string | undefined }) {
  const runtime = useStore((s) =>
    agentId ? s.agents.get(agentId)?.runtime : undefined
  );
  const usage = useStore((s) =>
    agentId ? s.agents.get(agentId)?.last_usage : undefined
  );

  if (!supportsContextUsage(runtime)) return null;

  const hit = usage?.cacheHitTokens;
  const total = usage?.cacheTotalInputTokens;
  if (
    hit === null ||
    hit === undefined ||
    total === null ||
    total === undefined ||
    total <= 0 ||
    hit < 0
  ) {
    return null;
  }

  // Preserve small, real hit rates instead of rounding (for example) 0.3% to
  // the misleading 0%. A malformed provider sample is capped at 100%; the raw
  // token counts remain visible in the tooltip for diagnosis.
  const pct = Math.min(100, (hit / total) * 100);
  const pctLabel =
    pct === 0
      ? "0%"
      : pct < 0.1
        ? "<0.1%"
        : pct < 10
          ? `${pct.toFixed(1)}%`
          : `${Math.round(pct)}%`;
  return (
    <span
      className="cw-cache"
      title={`缓存命中率：${formatTokens(hit)} / ${formatTokens(total)} 输入 tokens（${pctLabel}）`}
    >
      缓存命中 {pctLabel}
    </span>
  );
}
