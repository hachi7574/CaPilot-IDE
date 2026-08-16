import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";
import type { AcpItem } from "../../state/acpTypes";
import { ensureAgentChannel } from "../../state/agentActions";
import { isAcpRuntime } from "../../state/runtimeTransport";

function ItemRow({ item }: { item: AcpItem }) {
  if (item.kind === "message") {
    const role = item.role ?? "agent";
    return (
      <div className={`acp-msg acp-msg-${role}`}>
        <div className="acp-msg-role">
          {role === "user" ? "you" : role === "thought" ? "thought" : "assistant"}
        </div>
        <div className="acp-msg-body">{item.text}</div>
      </div>
    );
  }
  if (item.kind === "tool") {
    return (
      <div className={`acp-tool acp-tool-${item.status ?? "pending"}`}>
        <div className="acp-tool-title">
          <span className="acp-tool-kind">tool</span>
          <span>{item.text}</span>
          {item.status && <span className="acp-tool-status">{item.status}</span>}
        </div>
        {item.detail && <div className="acp-tool-detail">{item.detail}</div>}
      </div>
    );
  }
  if (item.kind === "plan") {
    return (
      <div className="acp-plan">
        <div className="acp-plan-label">plan</div>
        <pre className="acp-plan-body">{item.text}</pre>
      </div>
    );
  }
  if (item.kind === "permission") {
    return (
      <div className="acp-perm-inline">
        <span className="acp-perm-badge">permission</span> {item.text}
        {item.status && item.status !== "pending" && (
          <span className="acp-tool-status">{item.status}</span>
        )}
      </div>
    );
  }
  if (item.kind === "turn") {
    return (
      <div className="acp-turn-end">
        ── turn end ({item.text || item.status || "end_turn"}) ──
      </div>
    );
  }
  if (item.kind === "error") {
    return <div className="acp-error">{item.text}</div>;
  }
  if (item.kind === "stderr") {
    return <div className="acp-stderr">{item.text}</div>;
  }
  return <div className="acp-status">{item.text}</div>;
}

/**
 * Structured ACP session surface (not xterm). Renders the in-memory transcript
 * for one agent id; multi-tab isolation is enforced by filtering store events
 * on `agentId` upstream.
 */
export function AcpSessionPanel({
  agentId,
  active,
}: {
  agentId: string;
  active?: boolean;
}) {
  const agent = useStore((s) => s.agents.get(agentId));
  const session = useStore((s) => s.acpSessions.get(agentId));
  const resumeOnOpen = useStore((s) => s.resumeOnOpen.has(agentId));
  const consumeResume = useStore((s) => s.consumeResume);
  const listRef = useRef<HTMLDivElement>(null);
  const stickBottomRef = useRef(true);

  // Lazy resume when a restored ACP tab is opened.
  useEffect(() => {
    if (!agent || !isAcpRuntime(agent.runtime)) return;
    if (session?.live) return;
    if (agent.status === "done" || agent.status === "failed") return;
    let cancelled = false;
    (async () => {
      try {
        await ensureAgentChannel(agentId);
        if (!cancelled && resumeOnOpen) consumeResume(agentId);
      } catch (e) {
        console.error("ACP resume failed", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [agentId, agent?.runtime, agent?.status, session?.live, resumeOnOpen, consumeResume]);

  // Auto-scroll while the user is pinned to the bottom.
  useEffect(() => {
    const el = listRef.current;
    if (!el || !stickBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [session?.items.length, session?.pendingPermission?.requestId]);

  const onScroll = () => {
    const el = listRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickBottomRef.current = dist < 48;
  };

  const respond = async (outcome: "allow" | "reject") => {
    const req = session?.pendingPermission;
    if (!req) return;
    try {
      await invoke("acp_respond_permission", {
        id: agentId,
        requestId: req.requestId,
        outcome,
        optionId: outcome === "allow" ? "allow-once" : "reject-once",
      });
      // Clear pending locally; host will continue the turn.
      useStore.getState().applyAcpEvent({
        agentId,
        type: "status",
        status: "busy",
      });
      // Patch permission row status.
      const cur = useStore.getState().acpSessions.get(agentId);
      if (cur) {
        const acpSessions = new Map(useStore.getState().acpSessions);
        acpSessions.set(agentId, {
          ...cur,
          pendingPermission: null,
          items: cur.items.map((it) =>
            it.requestId === req.requestId
              ? { ...it, status: outcome === "allow" ? "allowed" : "rejected" }
              : it
          ),
        });
        useStore.setState({ acpSessions });
      }
    } catch (e) {
      console.error("acp_respond_permission failed", e);
    }
  };

  const items = session?.items ?? [];
  const title = agent?.title || agent?.runtime || "ACP";
  const turnActive = !!session?.turnActive;

  return (
    <div
      className={`acp-panel${active ? " active" : ""}`}
      data-agent-id={agentId}
      data-runtime={agent?.runtime ?? ""}
    >
      <div className="acp-panel-head">
        <span className="acp-panel-title">{title}</span>
        <span className="acp-panel-runtime">{agent?.runtime}</span>
        {turnActive && <span className="acp-panel-badge running">running</span>}
        {session?.pendingPermission && (
          <span className="acp-panel-badge waiting">permission</span>
        )}
        {session?.usage && (
          <span className="acp-panel-usage" title="context usage">
            {session.usage.used}/{session.usage.size}
          </span>
        )}
      </div>

      {session?.pendingPermission && (
        <div className="acp-perm-card">
          <div className="acp-perm-summary">{session.pendingPermission.summary}</div>
          <div className="acp-perm-actions">
            <button type="button" className="acp-perm-allow" onClick={() => void respond("allow")}>
              允许
            </button>
            <button type="button" className="acp-perm-reject" onClick={() => void respond("reject")}>
              拒绝
            </button>
          </div>
        </div>
      )}

      <div className="acp-scroll" ref={listRef} onScroll={onScroll}>
        {items.length === 0 && (
          <div className="acp-empty">
            {session?.live
              ? "ACP 会话已就绪 — 在下方输入框发送消息"
              : "正在连接 ACP agent…"}
          </div>
        )}
        {items.map((it) => (
          <ItemRow key={it.key} item={it} />
        ))}
      </div>
    </div>
  );
}
