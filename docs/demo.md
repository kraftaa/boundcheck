# Recorded demonstration

Recorded with `scripts/demo.sh` on macOS 26.7 (Darwin 25.6.0, arm64), Rust 1.98.0, Python 3.13.5 (Agents SDK venv) / Python 3.14.7 (fixtures).

The first run uses a real, unmodified runtime: OpenAI Agents SDK `openai-agents==0.23.1`. The later runs use the **faulty fixture**. Their failures are deliberately injected by `fixtures/faulty-agent/agent.py` and are not defects of any framework.

```text
$ boundarycheck run --adapter openai-agents-python --report boundarycheck-report.json -- .venv-agents/bin/python adapters/openai-agents-python/agent.py
boundarycheck 0.1.0
adapter openai-agents-python (adapters/openai-agents-python.json), protocol openai-chat-completions

PASS exact-text

PASS large-text-1k

PASS large-text-50k

PASS large-text-100k

PASS concurrent-two-tools

PASS sequential-history

PASS structured-json

PASS retry-429

PASS mcp-error

PASS unicode-boundaries

PASS replay-history

PASS persistence-resume

runtime: openai-agents 0.23.1 (version source: runtime-report-file)
Summary: 12 passed, 0 failed, 0 unknown
[exit status 0]

$ boundarycheck run --adapter fixture-agent --scenario exact-text --scenario large-text-100k --scenario concurrent-two-tools -- python3 fixtures/faulty-agent/agent.py --fault head-tail,swap
boundarycheck 0.1.0
adapter fixture-agent (adapters/fixture-agent.json), protocol openai-chat-completions

PASS exact-text

FAIL large-text-100k
  call:               BC_CALL_000001 (provider request #2)
  tool content:       102,400 bytes
  provider content:    49,999 bytes
  first difference:   byte 24,999
  missing sentinels:  25%, middle, 75%
  transport representation: changed
  extracted text bytes:     changed
  classification:     Truncation — head/tail retention (middle deleted)

FAIL concurrent-two-tools
  call:               BC_CALL_000001 (provider request #2)
  evidence:           tool_call_id BC_CALL_000001 carries the exact result of BC_CALL_000002
  transport representation: changed
  extracted text bytes:     changed
  classification:     WrongToolCallAssociation
  also: WrongToolCallAssociation BC_CALL_000002 (request #2): tool_call_id BC_CALL_000002 carries the exact result of BC_CALL_000001

runtime: boundarycheck-fixture-faulty 0.1.0 (version source: runtime-report-file)
Summary: 1 passed, 2 failed, 0 unknown
[exit status 1]

$ boundarycheck run --adapter fixture-agent --scenario retry-429 --scenario structured-json -- python3 fixtures/faulty-agent/agent.py --fault retry-mutation,json-float
boundarycheck 0.1.0
adapter fixture-agent (adapters/fixture-agent.json), protocol openai-chat-completions

FAIL retry-429
  call:               BC_CALL_000001 (provider request #3)
  tool content:       87 bytes
  provider content:   97 bytes
  first difference:   byte 87
  inserted text:      " [retried]"
  transport representation: changed
  extracted text bytes:     changed
  classification:     RetryMutation — inserted text

FAIL structured-json
  call:               BC_CALL_000001 (provider request #2)
  tool content:       528 bytes
  provider content:   540 bytes
  first difference:   byte 131
  first JSON change:  /order/big (number-changed)
  expected / actual:  12345678901234567890 / 1.2345678901234567e+19
  transport representation: changed
  extracted text bytes:     changed
  JSON semantics:           changed
  classification:     StructuralMutation

runtime: boundarycheck-fixture-faulty 0.1.0 (version source: runtime-report-file)
Summary: 0 passed, 2 failed, 0 unknown
[exit status 1]

```
