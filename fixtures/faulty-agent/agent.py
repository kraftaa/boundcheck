"""Faulty fixture runtime with deliberately injected, selectable faults.

Usage: agent.py --fault NAME[,NAME...] [--pid-file PATH]

Faults (all injected on purpose; none of them is a real framework defect):
  truncate          keep the first 50,000 bytes of long results
  head-tail         keep 25,000 head + 25,000 tail bytes of long results
  head-tail-marker  like head-tail, with "...[truncated]..." in between
  utf8-split        cut long results inside a multi-byte character (U+FFFD)
  swap              swap the contents of concurrent results
  duplicate         send every tool message twice
  missing           drop the last tool message of each turn
  retry-mutation    alter results only when re-sending after HTTP 429
  replay-mutation   alter persisted/replayed results after the first delivery
  normalize         apply Unicode NFC normalization to results
  json-float        re-encode JSON integers as floats (precision loss)
  reserialize-json  pretty-print JSON results (legal: semantics unchanged)
  json-dup-key      insert an earlier, different duplicate "order" key (last-key-wins parsers hide it)
  echo-user         also copy each result into an extra user message
  extra-id          add a tool message under an identifier that was never issued
  exit-early        exit right after receiving tool calls
  hang              block forever after receiving tool calls
  escape            start a child in its own session (setsid), record its pid, then hang
"""

import json
import os
import subprocess
import sys
import time
import unicodedata

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
import bc_agent  # noqa: E402

LONG = 50_000


def cut(text, limit):
    return text.encode("utf-8")[:limit].decode("utf-8", errors="ignore")


def tail(text, limit):
    raw = text.encode("utf-8")
    return raw[len(raw) - limit:].decode("utf-8", errors="ignore")


def utf8_split(text):
    raw = text.encode("utf-8")
    i = LONG
    while i < len(raw) and raw[i] < 0x80:
        i += 1
    return raw[: i + 1].decode("utf-8", errors="replace")  # keep a lone lead byte


def json_float(text):
    try:
        value = json.loads(text)
    except ValueError:
        return text

    def walk(v):
        if isinstance(v, bool):
            return v
        if isinstance(v, int):
            return float(v)
        if isinstance(v, list):
            return [walk(x) for x in v]
        if isinstance(v, dict):
            return {k: walk(x) for k, x in v.items()}
        return v

    return json.dumps(walk(value), ensure_ascii=False, separators=(",", ":"))


def reserialize(text):
    try:
        return json.dumps(json.loads(text), indent=2, sort_keys=True)
    except ValueError:
        return text


def json_dup_key(text):
    if not text.startswith("{") or '"order":' not in text:
        return text
    return '{"order":{"id":1},' + text[1:]


CONTENT_FAULTS = {
    "json-dup-key": json_dup_key,
    "truncate": lambda t: cut(t, LONG) if len(t.encode()) > LONG else t,
    "head-tail": lambda t: cut(t, 25_000) + tail(t, 25_000) if len(t.encode()) > LONG else t,
    "head-tail-marker": lambda t: cut(t, 25_000) + "\n...[truncated]...\n" + tail(t, 25_000) if len(t.encode()) > LONG else t,
    "utf8-split": lambda t: utf8_split(t) if len(t.encode()) > LONG else t,
    "normalize": lambda t: unicodedata.normalize("NFC", t),
    "json-float": json_float,
    "reserialize-json": reserialize,
}


class FaultyHooks(bc_agent.Hooks):
    def __init__(self, faults, pid_file):
        self.faults = faults
        self.pid_file = pid_file

    def tool_messages(self, messages):
        out = [dict(m) for m in messages]
        for name, fn in CONTENT_FAULTS.items():
            if name in self.faults:
                for m in out:
                    m["content"] = fn(m["content"])
        if "swap" in self.faults and len(out) >= 2:
            out[0]["content"], out[1]["content"] = out[1]["content"], out[0]["content"]
        if "missing" in self.faults:
            out = out[:-1]
        if "duplicate" in self.faults:
            out = [m for m in out for _ in (0, 1)]
        if "extra-id" in self.faults and out:
            out.append({"role": "tool", "tool_call_id": "BC_CALL_999999", "content": "injected by the runtime"})
        if "echo-user" in self.faults:
            out += [{"role": "user", "content": "Tool said: " + m["content"]} for m in messages]
        return out

    def retry_messages(self, messages, attempt):
        if "retry-mutation" not in self.faults:
            return messages
        mutated = [dict(m) for m in messages]
        for m in mutated:
            if m["role"] == "tool":
                m["content"] = m["content"] + " [retried]"
        return mutated

    def replay_messages(self, messages, resumed):
        if "replay-mutation" not in self.faults:
            return messages
        mutated = [dict(m) for m in messages]
        for m in mutated:
            if m["role"] == "tool":
                m["content"] = m["content"] + " [replayed]"
        return mutated

    def after_tool_calls(self):
        if "escape" in self.faults:
            child = subprocess.Popen(["sleep", "300"], start_new_session=True)
            with open(self.pid_file, "w") as f:
                f.write(str(child.pid))
            while True:
                time.sleep(3600)
        if self.pid_file:
            with open(self.pid_file, "w") as f:
                f.write(str(os.getpid()))
        if "exit-early" in self.faults:
            sys.exit(0)
        if "hang" in self.faults:
            while True:
                time.sleep(3600)


def main(argv):
    faults, pid_file = set(), None
    it = iter(argv)
    for arg in it:
        if arg == "--fault":
            faults |= set(next(it).split(","))
        elif arg == "--pid-file":
            pid_file = next(it)
    known = set(CONTENT_FAULTS) | {
        "swap", "missing", "duplicate", "retry-mutation", "replay-mutation", "exit-early", "hang", "echo-user", "extra-id", "escape",
    }
    unknown = faults - known
    if unknown or not faults:
        print(f"faulty-agent: choose --fault from {sorted(known)} (got {sorted(faults)})", file=sys.stderr)
        return 2
    return bc_agent.run("boundarycheck-fixture-faulty", hooks=FaultyHooks(faults, pid_file))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
