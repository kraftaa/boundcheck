"""Minimal, stdlib-only agent runtime used as a boundarycheck fixture.

It speaks MCP over stdio to the tool server and OpenAI Chat Completions over
HTTP to the provider. Fault hooks let the faulty fixture corrupt results at
exactly one place: between the MCP result and the provider request.
"""

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
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed stdout")
            reply = json.loads(line)
            if reply.get("id") == self.next_id:
                if "error" in reply:
                    raise RuntimeError(f"MCP error: {reply['error']}")
                return reply["result"]

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

    def after_tool_calls(self):
        pass


def post(base_url, api_key, body):
    req = urllib.request.Request(
        base_url.rstrip("/") + "/chat/completions",
        data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, dict(resp.headers), json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), None


def tool_result_text(result):
    return "".join(item["text"] for item in result.get("content", []) if item.get("type") == "text")


def write_runtime_info(name):
    path = os.environ.get("BOUNDARYCHECK_RUNTIME_INFO")
    if path:
        with open(path, "w") as f:
            json.dump({"name": name, "version": "0.1.0", "python": sys.version.split()[0]}, f)


def run(name, hooks=None, reorder=False):
    hooks = hooks or Hooks()
    write_runtime_info(name)
    base_url = os.environ["OPENAI_BASE_URL"]
    api_key = os.environ.get("OPENAI_API_KEY", "")
    mcp = McpClient(os.environ["BOUNDARYCHECK_MCP_COMMAND"], json.loads(os.environ["BOUNDARYCHECK_MCP_ARGS"]))
    try:
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
                status, headers, reply = post(
                    base_url, api_key, {"model": "fixture-model", "messages": body_messages, "tools": tools}
                )
                if status != 429:
                    break
                time.sleep(int(headers.get("retry-after-ms", "100")) / 1000)
                body_messages = hooks.retry_messages(messages, attempt + 1)
            if status != 200:
                print(f"provider returned HTTP {status}", file=sys.stderr)
                return 1
            message = reply["choices"][0]["message"]
            calls = message.get("tool_calls") or []
            if not calls:
                print(message.get("content"))
                return 0
            messages.append({"role": "assistant", "content": message.get("content"), "tool_calls": calls})
            hooks.after_tool_calls()
            results = []
            for call in calls:
                result = mcp.request(
                    "tools/call",
                    {"name": call["function"]["name"], "arguments": json.loads(call["function"]["arguments"])},
                )
                results.append({"role": "tool", "tool_call_id": call["id"], "content": tool_result_text(result)})
            if reorder:
                results.reverse()
            messages.extend(hooks.tool_messages(results))
        print("too many turns", file=sys.stderr)
        return 1
    finally:
        mcp.close()
