/** Frontend view-model for one ACP session (in-memory; not persisted). */

export type AcpItemKind =
  | "message"
  | "tool"
  | "plan"
  | "status"
  | "error"
  | "stderr"
  | "permission"
  | "turn";

export interface AcpItem {
  /** Stable row key within the session. */
  key: string;
  kind: AcpItemKind;
  /** `agent` | `user` | `thought` | `system` */
  role?: string;
  text: string;
  messageId?: string;
  toolCallId?: string;
  status?: string;
  detail?: string;
  requestId?: string;
  /** ISO-ish local stamp for optional display. */
  at: number;
}

export interface AcpPendingPermission {
  requestId: string;
  summary: string;
  toolCallId?: string;
}

export interface AcpSessionState {
  /** Process/host is live in AcpBridge. */
  live: boolean;
  /** A `session/prompt` is in flight. */
  turnActive: boolean;
  items: AcpItem[];
  pendingPermission: AcpPendingPermission | null;
  usage: { used: number; size: number } | null;
  lastStopReason: string | null;
}

export function emptyAcpSession(): AcpSessionState {
  return {
    // DEF-011b: restored / never-connected sessions are NOT live. Only
    // markAcpLive / session_started flip this true. Default true made
    // ensureAgentChannel skip resume while AcpBridge had no process.
    live: false,
    turnActive: false,
    items: [],
    pendingPermission: null,
    usage: null,
    lastStopReason: null,
  };
}

/** Wire payload from backend `AcpEventEnvelope` (`acp://event`). */
export type AcpEventPayload =
  | { agentId: string; type: "session_started"; sessionId: string; capabilities?: unknown; configOptions?: unknown; model?: string | null }
  | { agentId: string; type: "message_chunk"; messageId?: string; text: string; role?: string }
  | {
      agentId: string;
      type: "tool_call";
      toolCallId: string;
      title: string;
      kind?: string;
      status: string;
    }
  | {
      agentId: string;
      type: "tool_call_update";
      toolCallId: string;
      status: string;
      detail?: string;
    }
  | { agentId: string; type: "plan"; entries: unknown[] }
  | { agentId: string; type: "usage"; used: number; size: number }
  | {
      agentId: string;
      type: "permission_request";
      requestId: string;
      toolCallId?: string;
      summary: string;
      raw?: unknown;
    }
  | { agentId: string; type: "turn_done"; stopReason: string }
  | { agentId: string; type: "status"; status: string }
  | { agentId: string; type: "error"; message: string }
  | { agentId: string; type: "stderr"; line: string };
