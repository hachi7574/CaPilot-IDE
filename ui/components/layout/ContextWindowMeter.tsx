import { useStore } from "../../state/store";
import { supportsContextUsage } from "../../state/usageContext";

/** Compact token count: 412000 → "412K", 1_500_000 → "1.5M", 950 → "950". */
export function formatTokens(n: number): string {
  const abs = Math.abs(n);
  let v: number;
  let suffix = "";
  if (abs >= 1e9) {
    v = n / 1e9;
    suffix = "B";
  } else if (abs >= 1e6) {
    v = n / 1e6;
    suffix = "M";
  } else if (abs >= 1e3) {
    v = n / 1e3;
    suffix = "K";
  } else {
    return `${Math.round(n)}`;
  }
  // Whole numbers below 100 keep one decimal (1.5M); larger round cleanly.
  const rounded = v >= 100 ? Math.round(v) : Math.round(v * 10) / 10;
  return `${rounded}${suffix}`;
}

/**
 * Live context-window occupancy meter for the composer target line
 * (docs/context-window-usage.md).
 *
 * Rendering rules:
 *  - BOTH used and max present → determinate bar + percentage; the static
 *    model-picker capacity is NOT a fallback;
 *  - otherwise → nothing at all (no loading ring, no divider). The meter pops
 *    in only once its data has loaded; bash and other runtimes without a
 *    `context_usage` implementation never render anything.
 *
 * The divider (`cw-divider`) renders whenever the meter renders — it separates
 * the agent name from the meter in `.composer-target`.
 */
export function ContextWindowMeter({ agentId }: { agentId: string | undefined }) {
  const runtime = useStore((s) =>
    agentId ? s.agents.get(agentId)?.runtime : undefined
  );
  const usage = useStore((s) =>
    agentId ? s.agents.get(agentId)?.last_usage : undefined
  );

  // bash has no `context_usage` implementation (trait default `None`) — never
  // render a meter for it.
  if (!supportsContextUsage(runtime)) return null;

  const used = usage?.contextWindowUsedTokens;
  const max = usage?.contextWindowMaxTokens;
  const renderable =
    used !== null &&
    used !== undefined &&
    max !== null &&
    max !== undefined &&
    max > 0 &&
    used >= 0;

  if (!renderable) return null;

  const pct = Math.min(100, Math.max(0, Math.round((used! / max!) * 100)));
  // Warning from 70% used; destructive above 90% (per design spec).
  const tone = pct > 90 ? "danger" : pct >= 70 ? "warn" : "normal";
  return (
    <span
      className="cw-meter"
      title={`${formatTokens(used!)} / ${formatTokens(max!)} tokens (${pct}%)`}
    >
      <span className="cw-divider" aria-hidden="true" />
      <span className="cw-track" aria-hidden="true">
        <span className={`cw-fill tone-${tone}`} style={{ width: `${pct}%` }} />
      </span>
      <span className={`cw-pct tone-${tone}`}>{pct}%</span>
    </span>
  );
}
