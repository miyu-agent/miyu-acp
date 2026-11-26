#!/usr/bin/env python3
"""假的 ACP agent：集成测试用它扮 agent（不碰真的 agent 程序）。

收到的每一条消息、做过的几件事都追加写进 $FAKE_RECORD/<进程号>.jsonl（一行一条）。

FAKE_MODE（逗号隔开）定握手和开会话：
  sys=override|append|antigravity|none  系统提示词的能力（默认 override）
  auth                                   报两种登录方式；没用密钥 authenticate 之前 session/new 回 -32000
  models                                 session/new 带选模型的 configOptions（fast、smart）
  modes                                  session/new 报权限模式（老的 modes 和 configOptions 的 mode 两种写法），起在 auto：
                                         auto 时 EDIT 不要权限直接改（照 Claude 适配器没有设置时的样子）；session/set_mode、
                                         session/set_config_option 切到 default 以后才要权限
  noimage                                不收图

一轮做什么看这一次提问里最后一块文字里的记号（都没有的回一句 pong）：
  TOOL:<名字> <JSON>   报一件 mcp__miyu__<名字> 的 tool_call，经 MCP 服务调它，拿到结果以后说 got: <结果>
  SEARCH               报一件 WebSearch（kind search），要权限，放行了就交结果；说 searched: <选的>
  EDIT                 报一件 kind edit，要权限；说 edit: <选的>
  SILENT               报一件 kind execute，不要权限直接跑完
  STOP:<原因>           照这个 stopReason 结束
  FAIL:<原话>           session/prompt 回 JSON-RPC 出错
  LOGIN                session/prompt 回 -32000
  EXIT                 不回话直接退出
  SLOW                 等 session/cancel，回 cancelled
"""
import json
import os
import subprocess
import sys

MODE = set(filter(None, os.environ.get("FAKE_MODE", "").split(",")))
RECORD = os.environ.get("FAKE_RECORD")
PID = os.getpid()
SYS = next((flag.split("=", 1)[1] for flag in MODE if flag.startswith("sys=")), "override")

state = {"authenticated": False, "next": 1000, "mcp": None, "mcp_next": 1, "sessions": 0, "backlog": [], "mode": "auto"}
MODES = ["default", "acceptEdits", "auto", "bypassPermissions"]


def record(entry):
    if RECORD:
        with open(os.path.join(RECORD, f"{PID}.jsonl"), "a") as out:
            out.write(json.dumps(entry) + "\n")


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def reply(id, result):
    send({"jsonrpc": "2.0", "id": id, "result": result})


def error(id, code, message):
    send({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})


def update(session, body):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": body}})


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    message = json.loads(line)
    record({"got": message})
    return message


def ask(method, params):
    """向客户端发请求，读到它的回话为止；中间来的别的消息攒起来。"""
    state["next"] += 1
    id = state["next"]
    send({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    while True:
        message = read()
        if message.get("id") == id and "method" not in message:
            return message
        state["backlog"].append(message)


def initialize(id):
    session_caps = {"resume": {}}
    meta = {}
    if SYS == "override":
        session_caps["systemPrompt"] = {"mode": {"override": {}}}
    elif SYS == "append":
        session_caps["systemPrompt"] = {}
    elif SYS == "antigravity":
        meta = {"antigravity": {"systemPrompt": {"append": True}}}
    caps = {
        "loadSession": True,
        "promptCapabilities": {"image": "noimage" not in MODE, "embeddedContext": True},
        "mcpCapabilities": {"http": True},
        "sessionCapabilities": session_caps,
    }
    if meta:
        caps["_meta"] = meta
    methods = []
    if "auth" in MODE:
        methods = [{"id": "oauth-personal", "name": "Log in with Google"}, {"id": "gemini-api-key", "name": "Gemini API key"}]
    reply(id, {"protocolVersion": 1, "agentCapabilities": caps, "authMethods": methods,
               "agentInfo": {"name": "fake-acp", "title": "Fake Agent", "version": "0"}})


def start_mcp(servers):
    if not servers:
        return
    server = servers[0]
    env = dict(os.environ)
    for pair in server.get("env", []):
        env[pair["name"]] = pair["value"]
    proc = subprocess.Popen([server["command"]] + server.get("args", []), stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)
    state["mcp"] = proc
    mcp_request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}})
    tools = mcp_request("tools/list", {})["result"]["tools"]
    record({"tools": tools})


def mcp_request(method, params):
    proc = state["mcp"]
    id = state["mcp_next"]
    state["mcp_next"] += 1
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    proc.stdin.flush()
    while True:
        message = json.loads(proc.stdout.readline())
        if message.get("id") == id:
            return message


def new_session(id, params):
    if "auth" in MODE and not state["authenticated"]:
        error(id, -32000, "Authentication required")
        return
    state["sessions"] += 1
    session = f"sess-{PID}-{state['sessions']}"
    start_mcp(params.get("mcpServers", []))
    result = {"sessionId": session}
    options = []
    if "models" in MODE:
        options.append({"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "fast",
                        "options": [{"value": "fast", "name": "Fast"}, {"value": "smart", "name": "Smart"}]})
    if "modes" in MODE:
        result["modes"] = {"currentModeId": state["mode"], "availableModes": [{"id": mode, "name": mode} for mode in MODES]}
        options.append({"id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": state["mode"],
                        "options": [{"value": mode, "name": mode} for mode in MODES]})
    if options:
        result["configOptions"] = options
    reply(id, result)
    update(session, {"sessionUpdate": "available_commands_update", "availableCommands": []})


def permission(session, tool_call):
    options = [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
               {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
               {"optionId": "no", "name": "Reject", "kind": "reject_once"}]
    answer = ask("session/request_permission", {"sessionId": session, "toolCall": tool_call, "options": options})
    record({"permission": answer.get("result")})
    outcome = (answer.get("result") or {}).get("outcome", {})
    return outcome.get("optionId", outcome.get("outcome"))


def say(session, text):
    update(session, {"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"type": "text", "text": text}})


def finish(id, reason="end_turn", usage=True):
    result = {"stopReason": reason}
    if usage:
        result["usage"] = {"inputTokens": 11, "outputTokens": 7, "cachedReadTokens": 3, "cachedWriteTokens": 0, "totalTokens": 21}
    reply(id, result)


def prompt(id, params):
    session = params["sessionId"]
    texts = [block["text"] for block in params["prompt"] if block.get("type") == "text"]
    last = texts[-1] if texts else ""
    if "EXIT" in last:
        sys.exit(3)
    if "LOGIN" in last:
        error(id, -32000, "Authentication required")
        return
    if "FAIL:" in last:
        error(id, -32603, last.split("FAIL:", 1)[1].strip())
        return
    if "SLOW" in last:
        say(session, "working")
        while True:
            message = read()
            if message.get("method") == "session/cancel":
                reply(id, {"stopReason": "cancelled"})
                return
    update(session, {"sessionUpdate": "usage_update", "used": 4321, "size": 100000})
    if "TOOL:" in last:
        name, _, arguments = last.split("TOOL:", 1)[1].strip().partition(" ")
        arguments = json.loads(arguments.splitlines()[0] if arguments else "{}")
        say(session, "calling")
        update(session, {"sessionUpdate": "tool_call", "toolCallId": "t1", "name": f"mcp__miyu__{name}", "title": f"mcp__miyu__{name}", "kind": "other", "status": "pending"})
        chosen = permission(session, {"toolCallId": "t1", "name": f"mcp__miyu__{name}", "title": f"mcp__miyu__{name}"})
        result = mcp_request("tools/call", {"name": name, "arguments": arguments})["result"]
        record({"mcp_result": result})
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"})
        say(session, f"got({chosen}): " + "".join(item.get("text", "") for item in result["content"]))
        finish(id)
        return
    if "SEARCH" in last:
        call = {"toolCallId": "s1", "name": "WebSearch", "title": "Search the web", "kind": "search", "rawInput": {"query": "rust"}}
        update(session, dict(call, sessionUpdate="tool_call", status="pending"))
        chosen = permission(session, call)
        if chosen in ("yes", "always"):
            update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "s1", "status": "completed",
                             "content": [{"type": "content", "content": {"type": "text", "text": "1. Rust https://rust-lang.org"}}]})
        say(session, f"searched: {chosen}")
        finish(id)
        return
    if "EDIT" in last and "modes" in MODE and state["mode"] != "default":
        call = {"toolCallId": "e1", "title": "Edit notes.txt", "kind": "edit", "rawInput": {"path": "notes.txt"}}
        update(session, dict(call, sessionUpdate="tool_call", status="in_progress"))
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "e1", "status": "completed",
                         "content": [{"type": "diff", "path": "/tmp/notes.txt", "newText": "x"}]})
        say(session, f"edit: unasked in {state['mode']}")
        finish(id)
        return
    if "EDIT" in last:
        call = {"toolCallId": "e1", "title": "Edit notes.txt", "kind": "edit", "rawInput": {"path": "notes.txt"}}
        update(session, dict(call, sessionUpdate="tool_call", status="pending"))
        chosen = permission(session, call)
        if chosen in ("yes", "always"):
            update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "e1", "status": "completed",
                             "content": [{"type": "diff", "path": "/tmp/notes.txt", "newText": "x"}]})
        say(session, f"edit: {chosen}")
        finish(id)
        return
    if "SILENT" in last:
        update(session, {"sessionUpdate": "tool_call", "toolCallId": "x1", "title": "ls", "kind": "execute", "status": "in_progress"})
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": "x1", "status": "completed", "rawOutput": "a b c"})
        say(session, "ran it")
        finish(id)
        return
    if "STOP:" in last:
        say(session, "partial")
        finish(id, last.split("STOP:", 1)[1].split()[0], usage=False)
        return
    update(session, {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "thinking"}})
    images = sum(1 for block in params["prompt"] if block.get("type") == "image")
    say(session, f"pong {state['sessions']}: {last[-40:]}" + (f" [{images} image]" if images else ""))
    update(session, {"sessionUpdate": "plan", "entries": []})
    finish(id)


def handle(message):
    method, id, params = message.get("method"), message.get("id"), message.get("params") or {}
    if method == "initialize":
        initialize(id)
    elif method == "authenticate":
        key = (params.get("_meta") or {}).get("api-key")
        state["authenticated"] = bool(key)
        record({"authenticated_with_key": bool(key)})
        reply(id, {})
    elif method == "session/new":
        new_session(id, params)
    elif method == "session/set_mode" or (method == "session/set_config_option" and params.get("configId") == "mode"):
        mode = params.get("modeId", params.get("value"))
        if mode not in MODES:
            error(id, -32602, f"Invalid Mode: {mode}")
            return
        state["mode"] = mode
        record({"mode": mode})
        reply(id, {})
    elif method in ("session/set_config_option", "session/set_model"):
        reply(id, {})
    elif method == "session/prompt":
        prompt(id, params)
    elif method == "session/cancel":
        pass
    elif id is not None:
        error(id, -32601, f"method not found: {method}")


def main():
    record({"start": PID, "argv": sys.argv[1:], "cwd": os.getcwd()})
    while True:
        while state["backlog"]:
            handle(state["backlog"].pop(0))
        handle(read())


main()
