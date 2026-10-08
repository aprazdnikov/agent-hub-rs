"""Deterministic ACP fixture; no Hermes imports, models or external network calls."""
import json
import os
import pathlib
import sys
import time

case, raw_root = sys.argv[1:3]
root = pathlib.Path(raw_root)

def record(value):
    with (root / "wire.jsonl").open("a") as output:
        output.write(json.dumps(value) + "\n")

def send(value):
    print(json.dumps(dict(jsonrpc="2.0", **value)), flush=True)

def update(value):
    send({"method": "session/update", "params": {"sessionId": "s-1", "update": value}})

def chunk(text):
    update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})

def wait_release():
    deadline = time.monotonic() + 10
    while not (root / "release").exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("fixture release deadline expired")
        time.sleep(0.002)

record({"argv": sys.argv[3:], "cwd": os.getcwd()})
if case == "spawn_record":
    sys.exit(0)
if "--version" in sys.argv:
    if case == "version_error":
        print("0.21.5")
        sys.exit(1)
    print("0.21.4" if case == "old_version" else "0.21.5+9108.g61b7f95")
    sys.exit(0)
if "--check" in sys.argv:
    if case == "dependency_error":
        sys.exit(1)
    if case == "dependency_timeout":
        time.sleep(30)
    print("Hermes ACP check OK")
    sys.exit(0)

turn = 0
server = {}
for line in sys.stdin:
    message = json.loads(line)
    # Do not persist the ephemeral bearer token in fixture logs.
    logged = json.loads(line)
    for server in logged.get("params", {}).get("mcpServers", []):
        for header in server.get("headers", []):
            if header.get("name") == "Authorization":
                header["value"] = "Bearer <redacted>"
    record(logged)
    method = message.get("method")
    if method == "initialize":
        (root / "initialized").touch()
        if case == "initialize_hang":
            wait_release()
        if case == "initialize_eof":
            break
        send({"id": message["id"], "result": {
            "protocolVersion": 2 if case == "bad_protocol" else 1,
            "agentCapabilities": {
                "loadSession": case != "no_load",
                "promptCapabilities": {"image": case != "no_images"},
                "mcpCapabilities": {"http": case != "no_http"},
            },
        }})
    elif method in ("session/new", "session/load"):
        server = message["params"]["mcpServers"][0]
        if case == "load_error":
            send({"id": message["id"], "error": {"code": -32602, "message": "missing session"}})
        elif case == "load_null":
            send({"id": message["id"], "result": None})
        else:
            if method == "session/load":
                chunk("old replay text")
                update({"sessionUpdate": "tool_call", "toolCallId": "old", "title": "old replay tool", "kind": "execute"})
            send({"id": message["id"], "result": {"sessionId": "s-1"} if method == "session/new" else {}})
    elif method in ("session/set_model", "session/set_mode"):
        send({"id": message["id"], "result": None if case == "model_null" else {}})
    elif method == "session/prompt":
        turn += 1
        if case == "prompt_eof":
            break
        if case == "mcp":
            import urllib.request
            def mcp(method, params):
                body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
                headers = {header["name"]: header["value"] for header in server["headers"]}
                headers["Content-Type"] = "application/json"
                request = urllib.request.Request(server["url"], body, headers)
                with urllib.request.urlopen(request, timeout=5) as response:
                    return json.load(response)
            record({"mcp_list": mcp("tools/list", {})})
            record({"mcp_ask": mcp("tools/call", {"name": "ask_user", "arguments": {"questions": [{"question": "Fixture question?"}]}})})
            record({"mcp_file": mcp("tools/call", {"name": "send_file", "arguments": {"path": str(root / "report.txt")}})})
        chunk("first " if turn == 1 else "next ")
        update({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hidden reasoning"}})
        update({"sessionUpdate": "tool_call", "toolCallId": str(turn), "title": "fixture tool", "kind": "execute"})
        update({"sessionUpdate": "tool_call_update", "toolCallId": str(turn), "status": "completed"})
        # Estimated context pressure is not billed token usage.
        update({"sessionUpdate": "usage_update", "used": 999, "size": 1000})
        if case in ("queue", "stop_turn") and turn == 1:
            wait_release()
        if case.startswith("permission"):
            options = [
                {"optionId": "allow_always", "kind": "allow_always", "name": "Always"},
                {"optionId": "allow_once", "kind": "allow_once", "name": "Once"},
                {"optionId": "deny", "kind": "reject_once", "name": "Deny"},
            ]
            if case == "permission_invalid":
                options[1]["kind"] = "allow_always"
            send({"id": "permission", "method": "session/request_permission", "params": {
                "sessionId": "other" if case == "permission_wrong_session" else "s-1",
                "options": options,
                "toolCall": {"toolCallId": "perm", "title": "dangerous command", "kind": "execute"},
            }})
            reply = json.loads(sys.stdin.readline())
            record(reply)
        chunk("answer")
        send({"id": message["id"], "result": {
            "stopReason": "cancelled" if case == "cancelled_result" else "end_turn",
            "usage": {"inputTokens": 4, "outputTokens": 3, "totalTokens": 7} if case != "no_usage" else None,
        }})
    elif method == "session/cancel":
        break
    elif method:
        send({"id": message["id"], "error": {"code": -32601, "message": "unknown method"}})
