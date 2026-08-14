import { useEffect, useRef, useState } from "react";
import { Icon, runtimeIcon } from "../Icon";
import {
  useStructuredStore,
  AgentSnapshot,
  TimelineItem,
  PermissionRequest,
  ConfigOption,
  ConfigValue,
  AGENT_STATUS_TEXT,
  AgentStatus,
  agentConfigValue,
  refreshStructuredAgent,
  startStructuredTurn,
  interruptStructuredTurn,
  respondStructuredPermission,
  setStructuredConfig,
  closeStructuredAgent,
} from "../../state/structuredAgent";

/** Compact JSON for tool input/output panels (truncated preview). */
function fmtJson(value: unknown): string {
  try {
    const s = JSON.stringify(value, null, 2);
    return s.length > 6000 ? `${s.slice(0, 6000)}\n… (truncated)` : s;
  } catch {
    return String(value);
  }
}

function StatusBadge({ status }: { status: AgentStatus }) {
  return (
    <span className={`sa-status sa-${status}`}>
      {AGENT_STATUS_TEXT[status] ?? status}
    </span>
  );
}

/** One canonical timeline item (§6.2): message / reasoning / tool call / plan /
 *  error. Tool calls render collapsible input+output; messages preserve text. */
function TimelineRow({ item }: { item: TimelineItem }) {
  switch (item.kind) {
    case "user_message": {
      const m = item.data;
      return (
        <div className="sa-row sa-user">
          <div className="sa-row-head">
            <span className="sa-chip sa-chip-user">你</span>
            <span className="sa-time">{new Date(m.created_at).toLocaleTimeString()}</span>
          </div>
          <pre className="sa-text">{m.text}</pre>
        </div>
      );
    }
    case "assistant_message": {
      const m = item.data;
      return (
        <div className="sa-row sa-assistant">
          <div className="sa-row-head">
            <span className="sa-chip sa-chip-assistant">Agent</span>
            <span className="sa-time">{new Date(m.created_at).toLocaleTimeString()}</span>
          </div>
          <pre className="sa-text">{m.text}</pre>
        </div>
      );
    }
    case "reasoning": {
      const m = item.data;
      return (
        <div className="sa-row sa-reasoning">
          <details open={false}>
            <summary>
              <Icon name="moon" size={11} /> 思考 {m.text ? `· ${m.text.length} 字符` : ""}
            </summary>
            <pre className="sa-text">{m.text}</pre>
          </details>
        </div>
      );
    }
    case "tool_call": {
      const t = item.data;
      const statusText: Record<string, string> = {
        pending: "待执行",
        running: "执行中",
        completed: "已完成",
        failed: "失败",
        cancelled: "已取消",
      };
      return (
        <div className="sa-row sa-tool">
          <details open={t.status === "running" || t.status === "pending"}>
            <summary>
              <Icon name="wrench" size={11} />
              <span className="sa-tool-name">{t.tool_name}</span>
              <span className={`sa-tool-status sa-ts-${t.status}`}>
                {statusText[t.status] ?? t.status}
              </span>
            </summary>
            {t.tool_input !== undefined && t.tool_input !== null && (
              <div className="sa-json-block">
                <div className="sa-json-label">输入</div>
                <pre className="sa-json">{fmtJson(t.tool_input)}</pre>
              </div>
            )}
            {t.tool_output !== undefined && t.tool_output !== null && (
              <div className="sa-json-block">
                <div className="sa-json-label">输出</div>
                <pre className="sa-json">{fmtJson(t.tool_output)}</pre>
              </div>
            )}
          </details>
        </div>
      );
    }
    case "plan": {
      const p = item.data;
      return (
        <div className="sa-row sa-plan">
          <div className="sa-plan-title">
            <Icon name="list" size={11} /> {p.title}
          </div>
          <pre className="sa-text">{p.content}</pre>
        </div>
      );
    }
    case "error": {
      const e = item.data;
      return (
        <div className="sa-row sa-error">
          <Icon name="triangle-alert" size={12} />
          <span className="sa-error-msg">{e.message}</span>
        </div>
      );
    }
  }
}

/** One pending permission request: subject line + provider-native actions. */
function PermissionCard({
  agentId,
  req,
}: {
  agentId: string;
  req: PermissionRequest;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const act = (id: string) => {
    setBusy(id);
    respondStructuredPermission(agentId, req.id, id)
      .catch((e) => console.error("respond permission failed", e))
      .finally(() => setBusy(null));
  };
  return (
    <div className="sa-perm-card">
      <div className="sa-perm-head">
        <Icon name="shield" size={12} />
        <span className="sa-perm-title">{req.subject.title || req.title}</span>
      </div>
      {req.subject.description && (
        <div className="sa-perm-desc">{req.subject.description}</div>
      )}
      <div className="sa-perm-actions">
        {req.actions.map((a) => (
          <button
            key={a.id}
            className={`sa-perm-btn sa-pb-${a.behavior}${busy === a.id ? " busy" : ""}`}
            disabled={busy !== null}
            onClick={() => act(a.id)}
          >
            {a.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Model + provider config selector, driven by the provider catalog. */
function ConfigBar({
  agentId,
  snapshot,
  catalog,
}: {
  agentId: string;
  snapshot: AgentSnapshot;
  catalog?: ConfigSelectorCatalog;
}) {
  const models = catalog?.models ?? [];
  const options = catalog?.config_options ?? [];
  const configuredModel = agentConfigValue(snapshot.agent, "model");
  const model =
    (typeof configuredModel === "string" ? configuredModel : "") ||
    (models.find((m) => m.is_default)?.id ?? "") ||
    "";
  const setOpt = (configId: string, value: ConfigValue) => {
    setStructuredConfig(agentId, configId, value).catch((e) =>
      console.error("set config failed", e)
    );
  };
  return (
    <div className="sa-config">
      {models.length > 0 && (
        <label className="sa-config-item">
          <span className="sa-config-label">模型</span>
          <select
            value={model}
            onChange={(e) => setOpt("model", e.target.value)}
            title="切换模型（agent_set_config → model）"
          >
            {model === "" && <option value="">默认</option>}
            {models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        </label>
      )}
      {options.map((opt) =>
        opt.type === "select" ? (
          <label key={opt.id} className="sa-config-item">
            <span className="sa-config-label">{opt.label}</span>
            <select value={opt.current} onChange={(e) => setOpt(opt.id, e.target.value)}>
              {opt.options.map((o) => (
                <option key={o.id} value={o.id}>
                  {o.label}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <label key={opt.id} className="sa-config-item sa-config-toggle">
            <span className="sa-config-label">{opt.label}</span>
            <input
              type="checkbox"
              checked={opt.current}
              onChange={(e) => setOpt(opt.id, e.target.checked)}
            />
          </label>
        )
      )}
    </div>
  );
}

// Narrow catalog type for the config bar (avoids importing the full module type
// into the JSX layer's props repeatedly).
type ConfigSelectorCatalog = {
  models: { id: string; label: string; is_default: boolean }[];
  config_options: ConfigOption[];
};

/**
 * Unified Agent UI (architecture §14): canonical timeline + inline permission
 * panel + structured Composer. Mounted for `tab.type === "structured"`.
 *
 * Reconnect replay: on mount, if the view is missing from the store (agent
 * survived a GUI restart), fetch `agent_snapshot`; live events that raced the
 * fetch were buffered and are applied by the store's `setSnapshot`.
 */
export function AgentPanel({
  agentId,
}: {
  agentId: string;
  /** True for the panel of the active tab (F1 focus gating). */
  active?: boolean;
}) {
  const view = useStructuredStore((s) => s.agents.get(agentId));
  const providerId = view?.snapshot.agent.provider_id ?? "";
  const catalog = useStructuredStore((s) => s.catalogs[providerId] as ConfigSelectorCatalog | undefined);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // Reconnect replay: hydrate a missing view from the daemon snapshot.
  useEffect(() => {
    if (!view) {
      refreshStructuredAgent(agentId).catch((e) =>
        console.error("refresh structured agent failed", e)
      );
    }
  }, [view, agentId]);

  // Auto-scroll to the newest timeline entry as it streams.
  const timelineLen = view?.snapshot.timeline.length ?? 0;
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [timelineLen, view?.snapshot.timeline[timelineLen - 1]]);

  // F1: toggle focus between the structured composer and nothing else — keep
  // the input focused (structured agents have no PTY to hand focus to).
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "F1") return;
      e.preventDefault();
      inputRef.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!view) {
    return (
      <div className="content-panel sa-panel sa-loading">
        <div className="sa-loading-text">
          <Icon name="loader-circle" size={14} /> 正在恢复 Agent…
        </div>
      </div>
    );
  }

  const snap = view.snapshot;
  const agent = snap.agent;
  const status = agent.status;
  const busy = status === "running" || status === "waiting_permission";

  const submit = () => {
    const text = draft.trim();
    if (!text || sending) return;
    setSending(true);
    startStructuredTurn(agentId, text)
      .catch((e) => console.error("start turn failed", e))
      .finally(() => {
        setSending(false);
        setDraft("");
      });
  };

  return (
    <div className="content-panel sa-panel">
      <div className="sa-header">
        <span className="sa-provider">
          <Icon name={runtimeIcon(providerId)} size={13} />
          {providerId}
        </span>
        <StatusBadge status={status} />
        <span className="sa-model-tag">
          {agentConfigValue(agent, "model") ?? "默认模型"}
        </span>
        {busy && (
          <button
            className="sa-header-btn"
            title="中断当前回合"
            onClick={() => interruptStructuredTurn(agentId).catch(() => {})}
          >
            <Icon name="ban" size={12} /> 中断
          </button>
        )}
        <span className="sa-header-spacer" />
        <button
          className="sa-header-btn"
          title="关闭 Agent"
          onClick={() => closeStructuredAgent(agentId).catch(() => {})}
        >
          <Icon name="x" size={12} />
        </button>
      </div>

      {catalog && (catalog.models.length > 0 || catalog.config_options.length > 0) && (
        <ConfigBar
          agentId={agentId}
          snapshot={snap}
          catalog={catalog}
        />
      )}

      <div className="sa-scroll" ref={scrollRef}>
        {snap.timeline.length === 0 && (
          <div className="sa-empty">
            发送消息开始与 {providerId} Agent 对话。
          </div>
        )}
        {snap.timeline.map((item) => (
          <TimelineRow key={item.data.item_id} item={item} />
        ))}
        {status === "running" && (
          <div className="sa-typing">
            <Icon name="dot" size={12} /> Agent 工作中…
          </div>
        )}
      </div>

      {snap.pending_permissions.map((req) => (
        <PermissionCard key={req.id} agentId={agentId} req={req} />
      ))}

      <div className="sa-composer">
        <textarea
          ref={inputRef}
          className="sa-input"
          placeholder={`发送消息给 ${providerId} Agent（Enter 发送，Shift+Enter 换行）`}
          value={draft}
          rows={1}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
        />
        <button
          className="sa-send"
          disabled={!draft.trim() || sending}
          onClick={submit}
          title="发送"
        >
          <Icon name="send" size={14} />
        </button>
      </div>
    </div>
  );
}
