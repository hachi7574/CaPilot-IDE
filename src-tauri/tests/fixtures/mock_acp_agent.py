#!/usr/bin/env python3
"""Minimal ACP agent for CaPilot tests. NDJSON on stdio."""
from __future__ import annotations

import json
import sys
from typing import Any


def recv() -> dict[str, Any] | None:
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return recv()
    return json.loads(line)


def send(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notify_update(session_id: str, update: dict[str, Any]) -> None:
    send(
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session_id, "update": update},
        }
    )


def main() -> None:
    session_id = "sess_mock_1"
    next_agent_req = 9000
    while True:
        msg = recv()
        if msg is None:
            break
        mid = msg.get("id")
        method = msg.get("method")
        params = msg.get("params") or {}

        if method == "initialize":
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "protocolVersion": params.get("protocolVersion", 1),
                        "agentCapabilities": {
                            "loadSession": True,
                            "promptCapabilities": {
                                "image": False,
                                "audio": False,
                                "embeddedContext": True,
                            },
                        },
                        "agentInfo": {
                            "name": "mock-acp",
                            "title": "Mock ACP Agent",
                            "version": "0.0.1",
                        },
                        "authMethods": [],
                    },
                }
            )
        elif method == "session/new":
            session_id = "sess_mock_1"
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {
                        "sessionId": session_id,
                        "configOptions": [],
                    },
                }
            )
        elif method == "session/load":
            session_id = params.get("sessionId") or session_id
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {"sessionId": session_id, "configOptions": []},
                }
            )
        elif method == "session/prompt":
            session_id = params.get("sessionId") or session_id
            text = ""
            for block in params.get("prompt") or []:
                if block.get("type") == "text":
                    text += block.get("text") or ""
            lower = text.lower()

            # stream two chunks
            notify_update(
                session_id,
                {
                    "sessionUpdate": "agent_message_chunk",
                    "messageId": "msg_1",
                    "content": {"type": "text", "text": "echo:"},
                },
            )
            notify_update(
                session_id,
                {
                    "sessionUpdate": "agent_message_chunk",
                    "messageId": "msg_1",
                    "content": {"type": "text", "text": text[:200] or "ok"},
                },
            )

            # Optional: agent→client fs/read_text_file (host must sandbox).
            # Prompt containing "fsread:<abs-path>" triggers a client fs read.
            if "fsread:" in lower:
                idx = lower.index("fsread:")
                raw_path = text[idx + len("fsread:") :].strip().split()[0]
                next_agent_req += 1
                req_id = next_agent_req
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "method": "fs/read_text_file",
                        "params": {"path": raw_path},
                    }
                )
                fs_content = ""
                fs_err = None
                while True:
                    resp = recv()
                    if resp is None:
                        break
                    if resp.get("id") == req_id:
                        if "error" in resp:
                            fs_err = resp["error"].get("message", "fs error")
                        else:
                            fs_content = (resp.get("result") or {}).get("content") or ""
                        break
                if fs_err:
                    notify_update(
                        session_id,
                        {
                            "sessionUpdate": "agent_message_chunk",
                            "messageId": "msg_fs",
                            "content": {"type": "text", "text": f"fs_err:{fs_err}"},
                        },
                    )
                else:
                    notify_update(
                        session_id,
                        {
                            "sessionUpdate": "agent_message_chunk",
                            "messageId": "msg_fs",
                            "content": {
                                "type": "text",
                                "text": f"fs_ok:{fs_content[:80]}",
                            },
                        },
                    )

            if "permission" in lower:
                # ask client for permission (request). Monotonic agent-side ids
                # so concurrent/repeated prompts do not collide (DEF-005).
                next_agent_req += 1
                req_id = next_agent_req
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "call_mock_1",
                                "title": "Mock dangerous tool",
                                "kind": "execute",
                                "status": "pending",
                            },
                            "options": [
                                {
                                    "optionId": "allow-once",
                                    "name": "Allow once",
                                    "kind": "allow_once",
                                },
                                {
                                    "optionId": "reject-once",
                                    "name": "Reject",
                                    "kind": "reject_once",
                                },
                            ],
                        },
                    }
                )
                outcome = "cancelled"
                while True:
                    resp = recv()
                    if resp is None:
                        break
                    if resp.get("id") == req_id:
                        result = resp.get("result") or {}
                        oc = (result.get("outcome") or {}).get("outcome")
                        if oc == "selected":
                            oid = (result.get("outcome") or {}).get("optionId") or ""
                            outcome = "allowed" if "allow" in oid else "rejected"
                        else:
                            outcome = "cancelled"
                        break
                status = "completed" if outcome == "allowed" else "failed"
                notify_update(
                    session_id,
                    {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "call_mock_1",
                        "title": f"Mock dangerous tool ({outcome})",
                        "kind": "execute",
                        "status": status,
                    },
                )

            notify_update(
                session_id,
                {"sessionUpdate": "usage_update", "used": 100, "size": 100000},
            )
            send(
                {
                    "jsonrpc": "2.0",
                    "id": mid,
                    "result": {"stopReason": "end_turn"},
                }
            )
        elif method == "session/cancel":
            # OpenCode: cancel is a **notification** (no id). If a client wrongly
            # sends a request (with id), reject like OpenCode (-32601) so Host
            # tests catch DEF-002 regressions. Notification → silent OK.
            if mid is not None:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": mid,
                        "error": {
                            "code": -32601,
                            "message": "Method not found: session/cancel (use notification)",
                        },
                    }
                )
        elif method == "authenticate":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        else:
            if mid is not None:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": mid,
                        "error": {
                            "code": -32601,
                            "message": f"Method not found: {method}",
                        },
                    }
                )


if __name__ == "__main__":
    main()
