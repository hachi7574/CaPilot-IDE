//! Deterministic fake Codex `app-server` (conformance tests, architecture §8.2).
//!
//! Speaks the Codex app-server JSON-RPC protocol over NDJSON stdio as a real
//! server would: `initialize` → `thread/start` → on `turn/start` it streams a
//! scripted turn — a `commandExecution` item, an `item/commandExecution/requestApproval`
//! request it blocks on until the client answers, the completed tool output, an
//! agent message streamed as deltas, a token-usage update — then
//! `turn/completed`. `turn/interrupt` ends the outstanding turn with status
//! `interrupted`; `thread/unsubscribe` responds and exits.
//!
//! This binary is built only so `tests/contract_conformance.rs` has a
//! deterministic server to drive. It must never be bundled or reach production
//! paths.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut next_rpc_id: u64 = 9000;
    let mut thread_id = "fthread-1".to_string();
    let mut turn_active = false;
    // Pending permission: JSON-RPC id we asked the client to resolve.
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
                respond(&mut out, id.unwrap(), json!({}));
            }
            Some("thread/start") => {
                thread_id = "fthread-1".to_string();
                let thread = thread_value(&thread_id, "running");
                respond(&mut out, id.unwrap(), json!({ "thread": thread.clone() }));
                notify(
                    &mut out,
                    "thread/started",
                    json!({ "thread": thread, "threadId": thread_id }),
                );
            }
            Some("thread/resume") => {
                if let Some(tid) = msg
                    .get("params")
                    .and_then(|p| p.get("threadId"))
                    .and_then(Value::as_str)
                {
                    thread_id = tid.to_string();
                }
                let thread = thread_value(&thread_id, "running");
                respond(&mut out, id.unwrap(), json!({ "thread": thread }));
            }
            Some("model/list") => {
                respond(
                    &mut out,
                    id.unwrap(),
                    json!({
                        "data": [{
                            "id": "fake/codex-model",
                            "displayName": "Fake Codex Model",
                            "isDefault": true,
                            "supportedReasoningEfforts": [{ "id": "minimal" }]
                        }],
                        "nextCursor": null
                    }),
                );
            }
            Some("thread/settings/update") => {
                respond(&mut out, id.unwrap(), json!({}));
            }
            Some("turn/start") => {
                turn_active = true;
                respond(
                    &mut out,
                    id.unwrap(),
                    json!({ "turn": turn_value("inProgress") }),
                );
                notify(
                    &mut out,
                    "turn/started",
                    json!({ "threadId": thread_id, "turn": turn_value("inProgress") }),
                );
                stream_tool_turn(&mut out, &mut perm_rpc, &mut next_rpc_id);
            }
            Some("turn/interrupt") => {
                if let Some(pid) = perm_rpc.take() {
                    // Cancel the in-flight approval so the client never hangs on it.
                    respond(&mut out, pid, json!({ "decision": "cancel" }));
                }
                if turn_active {
                    turn_active = false;
                    notify(
                        &mut out,
                        "turn/completed",
                        json!({
                            "threadId": thread_id,
                            "turn": turn_value("interrupted"),
                        }),
                    );
                }
                respond(&mut out, id.unwrap(), json!({}));
            }
            Some("thread/unsubscribe") => {
                respond(&mut out, id.unwrap(), json!({ "status": "unsubscribed" }));
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
                        let decision = msg
                            .get("result")
                            .and_then(|r| r.get("decision"))
                            .and_then(Value::as_str)
                            .unwrap_or("decline");
                        match decision {
                            "cancel" => {
                                if turn_active {
                                    turn_active = false;
                                    notify(
                                        &mut out,
                                        "turn/completed",
                                        json!({
                                            "threadId": thread_id,
                                            "turn": turn_value("interrupted"),
                                        }),
                                    );
                                }
                            }
                            // accept / acceptForSession → full turn.
                            _ => finish_turn(&mut out),
                        }
                    }
                }
            }
        }
    }
}

/// Stream the scripted tool-call turn: command item + permission request.
fn stream_tool_turn(out: &mut impl Write, perm_rpc: &mut Option<u64>, next_rpc_id: &mut u64) {
    notify(
        out,
        "item/started",
        json!({
            "item": {
                "type": "commandExecution",
                "id": "fcx-call-1",
                "command": "date",
                "status": "inProgress",
                "cwd": "/tmp",
                "commandActions": []
            },
            "threadId": "fthread-1",
            "turnId": "fturn-1",
            "startedAtMs": 0
        }),
    );
    *perm_rpc = Some(*next_rpc_id);
    *next_rpc_id += 1;
    writeln!(
        out,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": perm_rpc.unwrap(),
            "method": "item/commandExecution/requestApproval",
            "params": {
                "command": "date",
                "availableDecisions": [
                    { "decision": "accept", "reason": "run the command" },
                    { "decision": "acceptForSession", "reason": "always allow" },
                    { "decision": "decline", "reason": "skip once" },
                    { "decision": "cancel", "reason": "cancel the turn" }
                ],
                "approvalId": null
            }
        })
    )
    .unwrap();
}

/// Complete the scripted turn after the permission is accepted.
fn finish_turn(out: &mut impl Write) {
    notify(
        out,
        "item/commandExecution/outputDelta",
        json!({ "itemId": "fcx-call-1", "delta": "2026-08-14\n" }),
    );
    notify(
        out,
        "item/completed",
        json!({
            "item": {
                "type": "commandExecution",
                "id": "fcx-call-1",
                "command": "date",
                "status": "completed",
                "cwd": "/tmp",
                "commandActions": [],
                "exitCode": 0,
                "aggregatedOutput": "2026-08-14\n"
            },
            "threadId": "fthread-1",
            "turnId": "fturn-1",
            "completedAtMs": 0
        }),
    );
    notify(
        out,
        "item/started",
        json!({
            "item": { "type": "agentMessage", "id": "fcx-msg-1", "text": "", "phase": "commentary" },
            "threadId": "fthread-1",
            "turnId": "fturn-1",
            "startedAtMs": 0
        }),
    );
    notify(
        out,
        "item/agentMessage/delta",
        json!({ "itemId": "fcx-msg-1", "delta": "Hello " }),
    );
    notify(
        out,
        "item/agentMessage/delta",
        json!({ "itemId": "fcx-msg-1", "delta": "world" }),
    );
    notify(
        out,
        "item/completed",
        json!({
            "item": { "type": "agentMessage", "id": "fcx-msg-1", "text": "Hello world", "phase": "final_answer" },
            "threadId": "fthread-1",
            "turnId": "fturn-1",
            "completedAtMs": 0
        }),
    );
    notify(
        out,
        "thread/tokenUsage/updated",
        json!({
            "threadId": "fthread-1",
            "turnId": "fturn-1",
            "tokenUsage": {
                "total": { "totalTokens": 100, "inputTokens": 60, "outputTokens": 40 },
                "modelContextWindow": 200000
            }
        }),
    );
    notify(
        out,
        "turn/completed",
        json!({ "threadId": "fthread-1", "turn": turn_value("completed") }),
    );
}

fn thread_value(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "status": status,
        "lastTurn": null,
        "workspaceId": null,
    })
}

fn turn_value(status: &str) -> Value {
    json!({ "id": "fturn-1", "status": status, "error": null })
}

fn notify(out: &mut impl Write, method: &str, params: Value) {
    writeln!(
        out,
        "{}",
        json!({ "jsonrpc": "2.0", "method": method, "params": params })
    )
    .unwrap();
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
