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
            if "permission" in text.lower():
                # ask client for permission (request)
                # Use a nested request id via separate channel - agent sends request
                # For mock: send request with id 9000+ 
                req_id = 9000 + (mid or 0)
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
                                {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                                {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                # wait for response
                while True:
                    resp = recv()
                    if resp is None:
                        break
                    if resp.get("id") == req_id:
                        break
                    # ignore other
                notify_update(
                    session_id,
                    {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "call_mock_1",
                        "title": "Mock dangerous tool",
                        "kind": "execute",
                        "status": "completed",
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
            # notification typically — if request, ack nothing required for notif
            pass
        elif method == "authenticate":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        else:
            if mid is not None:
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": mid,
                        "error": {"code": -32601, "message": f"Method not found: {method}"},
                    }
                )


if __name__ == "__main__":
    main()
