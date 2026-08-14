//! Deterministic fake ACP agent (conformance tests, architecture §8.1).
//!
//! Speaks ACP v1 over NDJSON stdio as a real agent would: `initialize` →
//! `session/new` → on `session/prompt` it streams a scripted turn — a tool call,
//! a `session/request_permission` request it blocks on until the client answers,
//! a completed tool output, two assistant chunks, and a usage update — then
//! responds to the prompt with `end_turn`. `session/cancel` (a notification)
//! terminates the outstanding turn with the `cancelled` stop reason; `session/close`
//! responds and exits.
//!
//! This binary is built only so `tests/acp_conformance.rs` has a deterministic
//! agent to drive. It must never be bundled or reach production paths.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut next_rpc_id: u64 = 9000;
    let mut prompt_rpc: Option<u64> = None;
    let mut perm_rpc: Option<u64> = None;

    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = msg.get("id").and_then(Value::as_u64);

        match method.as_deref() {
            Some("initialize") => {
                respond(
                    &mut out,
                    id.unwrap(),
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": true,
                            "sessionCapabilities": { "close": {}, "list": {}, "resume": {} },
                            "promptCapabilities": { "image": true },
                            "mcpCapabilities": {}
                        },
                        "authMethods": [],
                        "agentInfo": { "name": "fake-acp-agent", "version": "1.0" }
                    }),
                );
            }
            Some("session/new") => {
                respond(
                    &mut out,
                    id.unwrap(),
                    json!({
                        "sessionId": "fsess-1",
                        "configOptions": [
                            { "id": "model", "name": "Model", "category": "model", "type": "select",
                              "currentValue": "fake/model",
                              "options": [ { "value": "fake/model", "name": "Fake Model" } ] },
                            { "id": "verbose", "name": "Verbose", "category": "general",
                              "type": "boolean", "currentValue": false }
                        ]
                    }),
                );
            }
            Some("session/resume") => {
                respond(&mut out, id.unwrap(), json!({ "configOptions": [] }));
            }
            Some("session/prompt") => {
                let pid = id.expect("session/prompt must be a request");
                prompt_rpc = Some(pid);
                // Stream the tool-call turn.
                update(
                    &mut out,
                    json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": "fcall-1",
                        "title": "bash",
                        "kind": "execute",
                        "status": "pending",
                        "rawInput": { "command": "date" }
                    }),
                );
                update(
                    &mut out,
                    json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "fcall-1",
                        "status": "in_progress",
                        "rawInput": { "command": "date" }
                    }),
                );
                // Ask for permission; the client must respond before we finish.
                perm_rpc = Some(next_rpc_id);
                next_rpc_id += 1;
                writeln!(
                    out,
                    "{}",
                    json!({
                        "jsonrpc": "2.0",
                        "id": perm_rpc.unwrap(),
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "fsess-1",
                            "toolCall": { "toolCallId": "fcall-1", "toolTitle": "bash", "toolKind": "execute" },
                            "options": [
                                { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
                                { "optionId": "allow_always", "name": "Always allow", "kind": "allow_always" },
                                { "optionId": "reject_once", "name": "Reject once", "kind": "reject_once" },
                                { "optionId": "reject_always", "name": "Always reject", "kind": "reject_always" }
                            ],
                            "reason": "run `date`"
                        }
                    })
                )
                .unwrap();
            }
            Some("session/cancel") => {
                // Notification: terminate whatever is outstanding with `cancelled`.
                if let Some(pid) = perm_rpc.take() {
                    respond(
                        &mut out,
                        pid,
                        json!({ "outcome": { "outcome": "cancelled" } }),
                    );
                }
                if let Some(pid) = prompt_rpc.take() {
                    respond(&mut out, pid, json!({ "stopReason": "cancelled" }));
                }
            }
            Some("session/close") => {
                respond(&mut out, id.unwrap(), json!({}));
                break;
            }
            Some(other) => {
                if let Some(id) = id {
                    respond_error(&mut out, id, -32601, &format!("method not found: {other}"));
                }
            }
            None => {
                // A response from the client (e.g. our permission request).
                if let (Some(pid), Some(resp_id)) = (perm_rpc, id) {
                    if pid == resp_id {
                        perm_rpc = None;
                        finish_turn(&mut out, &mut prompt_rpc);
                    }
                }
            }
        }
    }
}

fn update(out: &mut impl Write, update: Value) {
    writeln!(
        out,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "fsess-1", "update": update } })
    )
    .unwrap();
}

fn finish_turn(out: &mut impl Write, prompt_rpc: &mut Option<u64>) {
    update(
        out,
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "fcall-1",
            "status": "completed",
            "content": [ { "type": "content", "content": { "type": "text", "text": "2026-08-14" } } ]
        }),
    );
    update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "fmsg-1",
            "content": { "type": "text", "text": "Hello " }
        }),
    );
    update(
        out,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "fmsg-1",
            "content": { "type": "text", "text": "world" }
        }),
    );
    update(
        out,
        json!({
            "sessionUpdate": "usage_update",
            "used": 100,
            "size": 200000,
            "cost": { "amount": 0, "currency": "USD" }
        }),
    );
    if let Some(pid) = prompt_rpc.take() {
        respond(
            out,
            pid,
            json!({
                "stopReason": "end_turn",
                "usage": { "inputTokens": 10, "outputTokens": 5, "totalTokens": 15, "cachedReadTokens": 0 }
            }),
        );
    }
}

fn respond(out: &mut impl Write, id: u64, result: Value) {
    writeln!(
        out,
        "{}",
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    )
    .unwrap();
}

fn respond_error(out: &mut impl Write, id: u64, code: i64, message: &str) {
    writeln!(
        out,
        "{}",
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
    )
    .unwrap();
}
