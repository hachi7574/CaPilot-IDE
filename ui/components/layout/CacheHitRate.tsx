/**
 * Session-cumulative cache hit rate chip (composer target line).
 *
 * Hidden for now — backend adapters still report `cacheHitTokens` /
 * `cacheTotalInputTokens`, so restoring the chip is a UI-only change.
 * Call site in Composer stays mounted for a stable layout slot.
 */
export function CacheHitRate({ agentId: _agentId }: { agentId: string | undefined }) {
  return null;
}
