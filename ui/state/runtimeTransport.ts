/**
 * Dual-track transport helpers.
 *
 * ACP runtime ids are always `acp:<name>` (e.g. `acp:opencode`). Never treat
 * the legacy PTY id `opencode` as ACP — Composer dialect controls (F12 / Ctrl+T
 * / Build·Plan) and ContentArea's resident xterm path must stay PTY-only.
 */

export type RuntimeTransport = "pty" | "acp";

/** True when the runtime id is an ACP agent (`acp:*`). */
export function isAcpRuntime(id: string | undefined | null): boolean {
  return typeof id === "string" && id.startsWith("acp:");
}

/** Prefer `RuntimeInfo.transport` when present; fall back to the id prefix. */
export function runtimeTransport(
  id: string | undefined | null,
  transportField?: string | null
): RuntimeTransport {
  if (transportField === "acp" || isAcpRuntime(id)) return "acp";
  return "pty";
}
