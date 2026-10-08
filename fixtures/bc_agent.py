"""Minimal, stdlib-only agent runtime used as a boundarycheck fixture.

It speaks MCP over stdio to the tool server and OpenAI Chat Completions over
HTTP to the provider. Fault hooks let the faulty fixture corrupt results at
exactly one place: between the MCP result and the provider request.
"""

import http.client
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request


class McpClient:
    def __init__(self, command, args):
        self.proc = subprocess.Popen(
            [command, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE
        )
        self.next_id = 0

    def request(self, method, params=None):
        return self.request_many([(None, method, params)])[0][1]

    def request_many(self, requests, tolerate_errors=False):
        pending = {}
        for key, method, params in requests:
            self.next_id += 1
            msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
            if params is not None:
                msg["params"] = params
            pending[self.next_id] = key
            self._send(msg)
        completed = []
        while pending:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed stdout")
            reply = json.loads(line)
            reply_id = reply.get("id")
            if reply_id in pending:
                if "error" in reply:
                    if not tolerate_errors:
                        raise RuntimeError(f"MCP error: {reply['error']}")
                    completed.append((pending.pop(reply_id), {"__error__": reply["error"]}))
                    continue
                completed.append((pending.pop(reply_id), reply["result"]))
        return completed

    def notify(self, method):
        self._send({"jsonrpc": "2.0", "method": method})

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg).encode() + b"\n")
        self.proc.stdin.flush()

    def initialize(self):
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "bc-fixture", "version": "0.1.0"},
            },
        )
        self.notify("notifications/initialized")

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


class Hooks:
    """Override in the faulty fixture. The conforming fixture uses these defaults."""

    def tool_messages(self, messages):
        return messages

    def retry_messages(self, messages, attempt):
        return messages

    def replay_messages(self, messages, resumed):
        return messages

    def after_tool_calls(self):
        pass

    def tool_result(self, result):
        return tool_result_content(result, PROTOCOL)


def post(url, api_key, body):
    req = urllib.request.Request(
        url,
        data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, dict(resp.headers), json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), None
    except (urllib.error.URLError, http.client.HTTPException, ConnectionError, OSError, ValueError) as e:
        # Connection closed or reset, truncated body, or unparseable JSON: retryable (status 0).
        print(f"provider connection error: {type(e).__name__}: {e}", file=sys.stderr)
        return 0, {}, None


RETRYABLE = {0, 429, 500, 502, 503}


class ChatTransport:
    """OpenAI Chat Completions. The conversation is kept in this shape internally."""

    def __init__(self, base_url, api_key):
        self.url = base_url.rstrip("/") + "/chat/completions"
        self.api_key = api_key

    def send(self, messages, tools):
        status, headers, reply = post(self.url, self.api_key, {"model": "fixture-model", "messages": messages, "tools": tools})
        return status, headers, (reply["choices"][0]["message"] if status == 200 else None)

    def committed(self, messages):
        pass

    def reset(self):
        pass


def to_items(messages):
    """Chat-shaped history -> Responses input items."""
    items = []
    for m in messages:
        if m.get("tool_calls"):
            if m.get("content"):
                items.append({"role": "assistant", "content": m["content"]})
            for c in m["tool_calls"]:
                items.append({"type": "function_call", "call_id": c["id"], "name": c["function"]["name"],
                              "arguments": c["function"]["arguments"]})
        elif m["role"] == "tool":
            output = m.get("content")
            if isinstance(output, list):
                output = [{"type": "input_text", "text": p["text"]} if p.get("type") in ("text", "input_text") else p
                          for p in output]
            item = {"type": "function_call_output", "output": output}
            if "tool_call_id" in m:
                item["call_id"] = m["tool_call_id"]
            items.append(item)
        else:
            items.append({"role": m["role"], "content": m.get("content") or ""})
    return items


class ResponsesTransport:
    """OpenAI Responses. With use_previous, only new items are sent after the
    first response, together with previous_response_id."""

    def __init__(self, base_url, api_key, use_previous=False):
        self.url = base_url.rstrip("/") + "/responses"
        self.api_key = api_key
        self.use_previous = use_previous
        self.prev_id = None
        self.sent = 0

    def send(self, messages, tools):
        body = {"model": "fixture-model", "tools": [
            {"type": "function", "name": t["function"]["name"], "description": t["function"].get("description", ""),
             "parameters": t["function"]["parameters"]} for t in tools]}
        if self.use_previous and self.prev_id is not None:
            body["previous_response_id"] = self.prev_id
            body["input"] = to_items(messages[self.sent:])
        else:
            body["input"] = to_items(messages)
        status, headers, reply = post(self.url, self.api_key, body)
        if status != 200:
            return status, headers, None
        self.prev_id = reply["id"]
        calls, text = [], None
        for item in reply["output"]:
            if item["type"] == "function_call":
                calls.append({"id": item["call_id"], "type": "function",
                              "function": {"name": item["name"], "arguments": item["arguments"]}})
            elif item["type"] == "message":
                text = "".join(c.get("text", "") for c in item["content"] if c.get("type") == "output_text")
        return status, headers, {"content": text, "tool_calls": calls or None}

    def committed(self, messages):
        # Everything up to here (including the echoed function calls) is stored server-side.
        self.sent = len(messages)

    def reset(self):
        self.prev_id = None
        self.sent = 0


def tool_result_content(result, protocol):
    """What the conforming runtime sends for one MCP result. Chat Completions
    carries text only; Responses also carries images, in order."""
    blocks = result.get("content", [])
    if protocol != "openai-responses" or "__error__" in result or not any(b.get("type") == "image" for b in blocks):
        return tool_result_text(result)
    parts = []
    for b in blocks:
        if b.get("type") == "text":
            parts.append({"type": "input_text", "text": b["text"]})
        elif b.get("type") == "image":
            parts.append({"type": "input_image", "image_url": f"data:{b.get('mimeType', 'image/png')};base64,{b['data']}"})
    return parts


def tool_result_text(result):
    """Text blocks concatenated in order; a JSON-RPC error becomes its message."""
    if "__error__" in result:
        err = result["__error__"]
        return f"MCP error {err.get('code')}: {err.get('message', '')}"
    return "".join(item["text"] for item in result.get("content", []) if item.get("type") == "text")


def write_runtime_info(name):
    path = os.environ.get("BOUNDARYCHECK_RUNTIME_INFO")
    if path:
        with open(path, "w") as f:
            json.dump({"name": name, "version": "0.1.0", "python": sys.version.split()[0]}, f)


def save_state(path, messages, tools):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0), 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump({"messages": messages, "tools": tools}, f, ensure_ascii=False, separators=(",", ":"))


def load_state(path):
    with open(path) as f:
        state = json.load(f)
    return state["messages"], state["tools"]


PROTOCOL = os.environ.get("BOUNDARYCHECK_PROVIDER_PROTOCOL", "openai-chat-completions")


def run(name, hooks=None, reorder=False, use_previous=False):
    hooks = hooks or Hooks()
    write_runtime_info(name)
    base_url = os.environ["OPENAI_BASE_URL"]
    api_key = os.environ.get("OPENAI_API_KEY", "")
    if PROTOCOL == "openai-responses":
        transport = ResponsesTransport(base_url, api_key, use_previous)
    else:
        transport = ChatTransport(base_url, api_key)
    resumed = os.environ.get("BOUNDARYCHECK_RESUME") == "1"
    state_path = os.environ.get("BOUNDARYCHECK_STATE_PATH", "")
    mcp = None
    try:
        if resumed:
            messages, tools = load_state(state_path)
            messages = hooks.replay_messages(messages, True)
        else:
            mcp = McpClient(os.environ["BOUNDARYCHECK_MCP_COMMAND"], json.loads(os.environ["BOUNDARYCHECK_MCP_ARGS"]))
            mcp.initialize()
            tools = [
                {
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t.get("description", ""),
                        "parameters": t["inputSchema"],
                    },
                }
                for t in mcp.request("tools/list")["tools"]
            ]
            messages = [{"role": "user", "content": os.environ["BOUNDARYCHECK_PROMPT"]}]
        for _turn in range(8):
            body_messages = messages
            for attempt in range(1, 5):
                status, headers, message = transport.send(body_messages, tools)
                if status not in RETRYABLE:
                    break
                time.sleep(int(headers.get("retry-after-ms", "100")) / 1000)
                body_messages = hooks.retry_messages(messages, attempt + 1)
            if status != 200:
                print(f"provider returned HTTP {status}", file=sys.stderr)
                return 1
            calls = message.get("tool_calls") or []
            if not calls:
                content = message.get("content")
                if content == "BOUNDARYCHECK_REPLAY_REQUIRED":
                    messages = hooks.replay_messages(messages, False)
                    transport.reset()  # a replay re-submits the whole history
                    continue
                if content == "BOUNDARYCHECK_CHECKPOINT_AND_EXIT":
                    save_state(state_path, messages, tools)
                    return 0
                print(content)
                return 0
            messages.append({"role": "assistant", "content": message.get("content"), "tool_calls": calls})
            transport.committed(messages)
            hooks.after_tool_calls()
            pending_calls = [
                (
                    call,
                    "tools/call",
                    {"name": call["function"]["name"], "arguments": json.loads(call["function"]["arguments"])},
                )
                for call in calls
            ]
            results = []
            for call, result in mcp.request_many(pending_calls, tolerate_errors=True):
                results.append({"role": "tool", "tool_call_id": call["id"], "content": hooks.tool_result(result)})
            if reorder:
                results.reverse()
            messages.extend(hooks.tool_messages(results))
        print("too many turns", file=sys.stderr)
        return 1
    finally:
        if mcp is not None:
            mcp.close()
