//! ACP v1 wire types (architecture §8.1).
//!
//! ACP is JSON-RPC 2.0 over NDJSON stdio: one JSON object per line. The client
//! and agent share a numeric `id` space; responses carry the request's `id`,
//! the agent pushes state with `session/update` notifications, and it pulls
//! permission decisions with `session/request_permission` requests (to which the
//! *client* must respond). Types here are the ACP schema only — conversion to
//! the provider-neutral domain model lives in `session.rs` and `mod.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// The only protocol version we speak.
pub const PROTOCOL_VERSION: u32 = 1;

/// JSON-RPC error object — shared transport type, re-exported for ACP callers.
pub use crate::agent_provider::rpc_stdio::RpcError;

// ── initialize (§5.1) ──────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientCapabilities {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: u32,
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default)]
    pub agent_info: Option<AgentInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
}

/// `{ close: {}, fork: {}, list: {}, resume: {} }` — the values are empty
/// objects, so presence (`Option`) is the signal, not truthiness.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default)]
    pub close: Option<Value>,
    #[serde(default)]
    pub fork: Option<Value>,
    #[serde(default)]
    pub list: Option<Value>,
    #[serde(default)]
    pub resume: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

// ── Session lifecycle ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams {
    pub cwd: PathBuf,
    /// Phase 2: the client configures no MCP servers; always `[]`.
    pub mcp_servers: Vec<Value>,
}

/// A config knob the agent reports after `session/new` / `session/resume`.
/// ACP calls these `configOptions`; select options carry the model list.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConfigOption {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type", default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub current_value: Option<Value>,
    #[serde(default)]
    pub options: Vec<AcpSelectOption>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSelectOption {
    pub value: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    pub session_id: String,
    #[serde(default)]
    pub config_options: Vec<AcpConfigOption>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionParams {
    pub session_id: String,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams {
    pub session_id: String,
}

// ── Prompt content blocks ──────────────────────────────────────

/// `session/prompt` `prompt` element. Text is the Phase 2 path; image/resource
/// pass through as opaque `Value`s (the wire names live with the provider).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: Value },
    Resource { resource: Value },
    Audio { source: Value },
}

/// Prompt-block shape as the agent sends it back inside `session/update`
/// (`tool_call_update.content[]` wraps the real block in `{type, content}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockIn {
    Text {
        text: String,
    },
    Image {
        source: Value,
    },
    Resource {
        resource: Value,
    },
    Audio {
        source: Value,
    },
    #[serde(other)]
    Other,
}

impl ContentBlockIn {
    /// The plain text of a text block (the only form Phase 2 renders).
    pub fn text(&self) -> Option<String> {
        match self {
            ContentBlockIn::Text { text } => Some(text.clone()),
            _ => None,
        }
    }
}

/// The `{ type: "content", content: {...} }` envelope opencode uses around a
/// completed tool call's output.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockEnvelope {
    pub content: ContentBlockIn,
}

// ── session/update notifications ───────────────────────────────

/// Tagged union on the `sessionUpdate` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "sessionUpdate",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionUpdate {
    UserMessageChunk {
        message_id: Option<String>,
        content: ContentBlockIn,
    },
    AgentMessageChunk {
        message_id: Option<String>,
        content: ContentBlockIn,
    },
    ThoughtChunk {
        message_id: Option<String>,
        content: ContentBlockIn,
    },
    ToolCall {
        tool_call_id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        #[serde(default)]
        raw_input: Option<Value>,
    },
    ToolCallUpdate {
        tool_call_id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        #[serde(default)]
        content: Option<Vec<ContentBlockEnvelope>>,
        #[serde(default)]
        raw_input: Option<Value>,
    },
    Plan {
        #[serde(default)]
        entries: Vec<PlanEntry>,
    },
    UsageUpdate {
        used: u64,
        size: u64,
        cost: Option<Value>,
    },
    AvailableCommandsUpdate {
        #[serde(default)]
        available_commands: Vec<Value>,
    },
    ModeChanged {
        mode: String,
        message_id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

// ── session/request_permission ─────────────────────────────────

/// The agent asks the client to resolve a permission. This arrives as a JSON-RPC
/// *request*; the client answers by responding to the same `id`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionParams {
    pub session_id: String,
    pub tool_call: RequestedToolCall,
    #[serde(default)]
    pub options: Vec<PermissionOption>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedToolCall {
    pub tool_call_id: String,
    #[serde(default)]
    pub tool_title: Option<String>,
    #[serde(default)]
    pub tool_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Response to a `session/request_permission` request: pick one of the offered
/// options, or cancel the whole turn.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResponse {
    pub outcome: PermissionOutcome,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOutcome {
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_result_deserializes() {
        let raw = r#"{
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": { "close": {}, "fork": {}, "list": {}, "resume": {} },
                "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
                "mcpCapabilities": { "http": true, "sse": false }
            },
            "authMethods": [{ "id": "opencode-login", "name": "Login with opencode" }],
            "agentInfo": { "name": "OpenCode", "version": "1.18.18" }
        }"#;
        let result: InitializeResult = serde_json::from_str(raw).unwrap();
        assert_eq!(result.protocol_version, 1);
        assert!(result.agent_capabilities.load_session);
        assert!(result
            .agent_capabilities
            .session_capabilities
            .resume
            .is_some());
        assert!(result
            .agent_capabilities
            .session_capabilities
            .close
            .is_some());
        assert!(result.agent_capabilities.prompt_capabilities.image);
        assert!(result.agent_capabilities.mcp_capabilities.http);
        assert_eq!(result.auth_methods.len(), 1);
        assert_eq!(result.agent_info.unwrap().name.unwrap(), "OpenCode");
    }

    #[test]
    fn initialize_params_serializes_camel_case() {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "capilot-ide".into(),
                title: Some("CaPilot IDE".into()),
                version: Some("0.1.0".into()),
            },
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["protocolVersion"], 1);
        assert_eq!(v["clientInfo"]["name"], "capilot-ide");
        assert_eq!(v["clientInfo"]["title"], "CaPilot IDE");
    }

    #[test]
    fn session_update_roundtrip() {
        let raw = r#"{
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "done" } }
            ]
        }"#;
        let u: SessionUpdate = serde_json::from_str(raw).unwrap();
        match u {
            SessionUpdate::ToolCallUpdate {
                tool_call_id,
                status,
                content,
                ..
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(status.as_deref(), Some("completed"));
                let c = content.unwrap();
                assert_eq!(c[0].content.text().as_deref(), Some("done"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn session_update_unknown_tag_is_unknown() {
        let raw = r#"{"sessionUpdate": "something_new", "foo": 1}"#;
        let u: SessionUpdate = serde_json::from_str(raw).unwrap();
        assert!(matches!(u, SessionUpdate::Unknown));
    }

    #[test]
    fn request_permission_deserializes() {
        let raw = r#"{
            "sessionId": "s1",
            "toolCall": { "toolCallId": "call_1", "toolTitle": "bash", "toolKind": "execute" },
            "options": [
                { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
                { "optionId": "allow_always", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "reject_once", "name": "Reject once", "kind": "reject_once" }
            ],
            "reason": "run a command"
        }"#;
        let p: RequestPermissionParams = serde_json::from_str(raw).unwrap();
        assert_eq!(p.tool_call.tool_call_id, "call_1");
        assert_eq!(p.tool_call.tool_kind.as_deref(), Some("execute"));
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[1].kind.as_deref(), Some("allow_always"));
        assert_eq!(p.reason.as_deref(), Some("run a command"));
    }

    #[test]
    fn new_session_result_deserializes() {
        let raw = r#"{
            "sessionId": "ses_1",
            "configOptions": [
                { "id": "model", "name": "Model", "category": "model", "type": "select",
                  "currentValue": "opencode/big-pickle",
                  "options": [ { "value": "opencode/big-pickle", "name": "Big Pickle" } ] },
                { "id": "verbose", "name": "Verbose", "type": "boolean", "currentValue": false }
            ]
        }"#;
        let r: NewSessionResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.session_id, "ses_1");
        assert_eq!(r.config_options.len(), 2);
        assert_eq!(r.config_options[0].id, "model");
        assert_eq!(
            r.config_options[0].current_value.as_ref().unwrap(),
            "opencode/big-pickle"
        );
    }

    #[test]
    fn permission_response_serializes() {
        let resp = PermissionResponse {
            outcome: PermissionOutcome {
                outcome: "selected".into(),
                option_id: Some("allow_once".into()),
            },
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["outcome"]["outcome"], "selected");
        assert_eq!(v["outcome"]["optionId"], "allow_once");

        let cancelled = PermissionResponse {
            outcome: PermissionOutcome {
                outcome: "cancelled".into(),
                option_id: None,
            },
        };
        let v = serde_json::to_value(&cancelled).unwrap();
        assert_eq!(v["outcome"]["outcome"], "cancelled");
        assert!(v["outcome"].get("optionId").is_none());
    }
}
