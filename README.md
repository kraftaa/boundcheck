# boundarycheck

`boundarycheck` is a deterministic command-line test harness. It answers one question with reproducible evidence:

> The tool produced X. What did the agent runtime send to the model provider?

It places a fake MCP tool server on one side of the runtime under test and a fake model provider on the other. It records both boundaries independently, then compares them:

```text
Fake MCP tool server (stdio)   ── records every byte it writes to stdout
        ↓
Agent runtime under test       ── launched through an adapter
        ↓
Fake model provider (HTTP)     ── records every raw request body
```

No real model is called. Runtime tracing is not used as ground truth, and no LLM classifies or explains anything. Each scenario ends as `PASS`, `FAIL` or `UNKNOWN`. A `FAIL` is reported only when the harness can prove it. If it cannot, the result is `UNKNOWN`.

> **Intended use.** Use boundarycheck on controlled test agents in a test environment. It starts the command you give it, with environment variables that point the runtime at local fake endpoints.

## Quick start

```bash
cargo build --release
BC=./target/release/boundarycheck

# Conforming fixture runtime: every scenario passes
$BC run --adapter fixture-agent -- python3 fixtures/conforming-agent/agent.py

# Faulty fixture runtime with deliberately injected faults
$BC run --adapter fixture-agent -- python3 fixtures/faulty-agent/agent.py --fault head-tail,swap

# Real runtime: OpenAI Agents SDK (pinned)
uv venv .venv-agents --python 3.13
VIRTUAL_ENV=.venv-agents uv pip install -r adapters/openai-agents-python/requirements.txt
$BC run --adapter openai-agents-python --report boundarycheck-report.json \
  -- .venv-agents/bin/python adapters/openai-agents-python/agent.py
```

`scripts/demo.sh` runs all three. A recorded run is in [docs/demo.md](docs/demo.md).

Example output (faulty fixture):

```text
FAIL large-text-100k
  call:               BC_CALL_000001 (provider request #2)
  tool content:       102,400 bytes
  provider content:    49,999 bytes
  first difference:   byte 24,999
  missing sentinels:  25%, middle, 75%
  transport representation: changed
  extracted text bytes:     changed
  classification:     Truncation — head/tail retention (middle deleted)
```

## Command line

```text
boundarycheck run --adapter <name|path> [options] -- <agent-command> [args...]
boundarycheck list-scenarios
boundarycheck list-adapters
```

| Flag | Meaning |
|---|---|
| `--adapter <name>` | Required. A built-in adapter name, `./adapters/<name>.json`, or a path to a manifest. |
| `--report <path>` | Write the JSON report to this path. |
| `--artifacts <path>` | Save evidence for FAIL and UNKNOWN scenarios under `<path>/<run_id>/`. This also keeps the work directory. |
| `--artifacts-all` | With `--artifacts`, also save evidence for passing scenarios. |
| `--timeout <seconds>` | Per-scenario timeout, including startup. Default 60. |
| `--scenario <name>` | Run only this scenario. Repeatable. By default every scenario supported by the selected adapter runs. |
| `--keep-workdir` | Keep the temporary work directory and print its path. |
| `--save-headers` | Save provider request headers with the artifacts. Credentials are always redacted. |

boundarycheck runs the agent command directly, without a shell. Ports are allocated dynamically on `127.0.0.1` and are never exposed as flags. Each scenario starts a fresh runtime process; `persistence-resume` deliberately starts a second process from persisted state.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Every selected scenario passed. |
| 1 | At least one scenario failed. |
| 2 | Harness, adapter or configuration error, for example an unknown adapter, a command that cannot be launched, or a provider that cannot bind. Also returned on Ctrl-C. |
| 3 | No failures, but at least one scenario was unknown. |

## Provider protocol (V1): OpenAI Chat Completions

V1 implements exactly one protocol: **OpenAI Chat Completions**. Other protocols, including the Responses API, are not supported. An adapter that declares any other `provider_protocol` is rejected with exit code 2.

**Endpoint.** The provider base URL given to the runtime is `http://127.0.0.1:<port>/<run_id>/<scenario_id>/v1`. Supported endpoints:

- `POST {base}/chat/completions`, the protocol under test.
- `GET {base}/models`, which returns one model, `boundarycheck-model`.

Any other path gets HTTP 404 and makes the scenario UNKNOWN. A request for a different run or scenario path gets HTTP 403, and its body is discarded without being stored.

**Supported request shape.** The request is a JSON object with a `messages` array. Each message has a `role`.

- The tool offered to the model is read from `tools[].function.name`, where `type` is `"function"` or absent. The name must be `boundary_test`, or exactly one name that ends in `<separator>boundary_test` (for example `server__boundary_test`). No other matching is done.
- A tool result is a message with `role: "tool"` and `tool_call_id`. Its `content` is either
  - a string, or
  - an array of `{"type": "text", "text": ...}` parts. The provider-visible text is the parts concatenated in order.

  Any other content part, such as an image, makes the scenario UNKNOWN (`unsupported-request-shape`).
- The fields `model`, `stream`, `stream_options.include_usage` and `parallel_tool_calls` are read. All other fields are ignored.
- If the body is not valid UTF-8, it is parsed lossily. Any resulting difference is classified `InvalidUtf8`.

**Responses.** Responses are deterministic: fixed IDs `chatcmpl-boundarycheck-NNNNNN`, a fixed `created` value, and zero usage.

- If `stream` is false or absent, the response is a `chat.completion` object.
- If `stream` is true, the response is `text/event-stream` with `chat.completion.chunk` events and a final `data: [DONE]`. A usage chunk is added when `stream_options.include_usage` is set.
- Tool calls use the deterministic call IDs below. Their `arguments` are `{"run_id":…,"scenario":…,"call_id":…}`.
- The final answer has the content `BOUNDARYCHECK_SCENARIO_COMPLETE`.

Replay-capable adapters also understand two deterministic control responses. `BOUNDARYCHECK_REPLAY_REQUIRED` asks for the completed history again in the same process. `BOUNDARYCHECK_CHECKPOINT_AND_EXIT` asks the runtime to persist that history and exit; the harness then launches the adapter's `resume` process.

**Retry scenario.** The first tool-result request gets HTTP 429 with `retry-after-ms: 50` and `retry-after: 1` headers and an `error.type` of `rate_limit_error`.

**Error status.** Chat Completions tool messages have no error flag. An MCP `isError: true` result can therefore only be checked by its content. The report shows `error_status: "not-representable"`.

## MCP server

`boundarycheck mcp-server` is a hidden subcommand. The runtime launches it through the adapter's `{{mcp_command}}` / `{{mcp_args_json}}`.

- **Transport.** It uses newline-delimited JSON-RPC 2.0 over stdio. Protocol messages go to stdout and diagnostics to stderr. It exits cleanly on EOF.
- **Methods.** It implements `initialize` (protocol versions `2025-06-18`, `2025-03-26`, `2024-11-05`), `ping`, `tools/list`, `tools/call`, and empty `resources/list` and `prompts/list`.
- **Tool.** It exposes one tool, `boundary_test`, with arguments `{run_id, scenario, call_id}`.
- **Payloads.** Payloads depend only on those three values. They contain no clocks or random values.
- **Evidence.** Every response is appended to an evidence log as base64 of the exact bytes, under an exclusive file lock, before it is written to stdout. If the arguments do not match the active run, the server returns an `isError` result instead of a payload, and the scenario becomes UNKNOWN (`tool-arguments-changed`).

## Scenarios

| Scenario | What it checks |
|---|---|
| `exact-text` | A small UTF-8 text with characters that need JSON escaping (`"`, `\`, tab, U+0001, U+2028) arrives byte-identical. |
| `large-text-1k` / `-50k` / `-100k` | 1,024 / 51,200 / 102,400 bytes of mixed 1–4 byte UTF-8. Sentinels `[[BC_SENTINEL:NNN:<call>]]` start at exactly 0%, 25%, 50% and 75%, and the last one ends at 100%. |
| `concurrent-two-tools` | Two calls are put in flight together: `BC_CALL_000001` → `city=Boston`, `BC_CALL_000002` → `city=Chicago`. Call A is delayed so B completes first; results must remain associated by ID. |
| `sequential-history` | Two calls in consecutive turns. Every later request must still contain each earlier result exactly once and unchanged. |
| `structured-json` | Nested JSON with `9007199254740993`, `12345678901234567890`, `19.990`, `1e-7`, `-0.0`, unicode keys and deep nesting. Compared semantically. |
| `retry-429` | The result request gets HTTP 429 once. The retried request must carry the same result. |
| `mcp-error` | An `isError: true` result. Its content must arrive unchanged. |
| `unicode-boundaries` | NFC/NFD pairs, stacked and leading combining marks, ZWJ emoji, flags, VS16, RTL, Devanagari, Hangul jamo, astral characters, a BOM, ZWNJ, fullwidth characters and ligatures. Byte-exact. |
| `replay-history` | The completed tool-result history is submitted a second time by the same process and must remain unchanged. Requires adapter capability `history-replay`. |
| `persistence-resume` | The runtime persists its completed history, exits, and a new process reloads and submits it. Requires adapter capability `persistence-resume`. |

The identifiers are deterministic:

- `run_id` is `BC_RUN_000001`. With `--artifacts`, it is the next unused number in that directory.
- Call IDs are `BC_CALL_000001` and `BC_CALL_000002`.
- The scenario ID is the scenario name.

Every payload contains its own `run_id|scenario|call_id`. This gives independent evidence of association.

Each scenario is a finite state machine on the provider side:

```text
WAIT_INITIAL_REQUEST --(tools offered)--> send tool call(s)
WAIT_TOOL_RESULT(turn) --(result request)--> [429 once for retry-429] --> next turn | final answer
WAIT_TOOL_RESULT --(replay scenario)--> WAIT_REPLAYED_HISTORY | WAIT_RESUMED_HISTORY
COMPLETE
anything unexpected --> ABORTED  (the scenario becomes UNKNOWN, never a guessed FAIL)
```

## How results are compared

Evidence is kept at three levels, and they are reported separately:

1. **Transport.** These are the raw MCP response line and the raw provider request body. They always differ, because the envelopes differ. boundarycheck also compares the raw JSON string token of the content on each side. For example, `"caf\u00e9"` and `"café"` are the same text with a different transport representation. The SDK's one-part content array is also reported as `changed`.
2. **Extracted content.** These are the logical text bytes on each side, compared byte for byte and with SHA-256. Error status, call ID, ordering and occurrence count are recorded alongside.
3. **Semantic value.** This applies to `structured-json` only. Both sides are parsed and compared canonically:
   - objects are unordered;
   - numbers are compared by exact decimal value, so `19.990 == 19.99` and `-0 == 0`, but `9007199254740993 != 9007199254740992`;
   - strings are not Unicode-normalized.

   The first changed JSON Pointer path is reported. Re-serialized but semantically identical JSON is a PASS, with a note.

### Correlation rules

- The provider-protocol `tool_call_id` is the primary key. Results are never matched by text similarity.
- Every request after a tool call is checked. It must contain exactly one tool message per issued call, with exactly the content that MCP emitted. This also catches mutations that appear only later in the history.
- Order does not matter. A different order is reported only as a note.
- If an ID's content is exactly another call's MCP content, the result is `WrongToolCallAssociation`, not a content mutation.
- If no tool message has the ID but the exact content appears under another ID or without one, the result is `WrongToolCallAssociation`. If it appears inside a non-tool message, the result is `MissingResult` with that evidence.
- A tool result under an unissued or missing `tool_call_id` that matches no MCP emission makes the scenario UNKNOWN (`ambiguous-correlation`); it is never silently accepted.

### Failure classes

Classes are derived from structural facts only: the common prefix and suffix, exact containment, exact repetition, and Unicode-normalization equality.

| Class | Proven by |
|---|---|
| `MissingResult` | An issued call has no tool message in a later request. |
| `DuplicateResult` | More than one tool message has the same ID, the content is the original repeated exactly, the exact result is also copied into a non-tool message, or it is sent again under an unissued ID. |
| `WrongToolCallAssociation` | One call's exact result appears under another ID, or under no ID. |
| `Truncation` | The provider content is shorter and is a prefix, a suffix, a head+tail (possibly with an inserted marker), an interior slice, or empty. |
| `InvalidUtf8` | A U+FFFD was inserted at the boundary, or the request body was invalid UTF-8. |
| `InvalidJson` | JSON scenario: the provider content no longer parses. |
| `StructuralMutation` | JSON scenario: the canonical values differ (the first path is reported), or an object key appears twice, whose meaning depends on the parser. |
| `RetryMutation` | The retried request differs from the MCP result, although attempt 1 matched it. |
| `ContentMutation` | Any other proven difference: Unicode normalization (NFC/NFD/NFKC/NFKD), inserted text, or replaced text. |
| `ReplayMutation` | A result that arrived intact originally is missing, duplicated, reassociated or changed in replayed or restored history. |
| `ErrorStatusMutation` | Reserved because Chat Completions cannot represent the MCP error flag. |

The human output reports the most specific proven fact. All findings are in the JSON report.

### UNKNOWN reasons

| Reason | When |
|---|---|
| `unsupported-request-shape` | An unsupported endpoint, or tool content that is neither a string nor text parts. |
| `ambiguous-correlation` | More than one offered tool could be `boundary_test`. |
| `runtime-exited-early` | The runtime exited before the state machine completed. |
| `startup-timeout` | No provider request arrived within the adapter's readiness timeout. |
| `scenario-timeout` | The scenario did not complete within `--timeout`. |
| `provider-request-unparseable` | The body is not JSON, has no `messages`, or exceeds 32 MiB. |
| `adapter-insufficient` | The MCP server was never started, or the `boundary_test` tool was never offered to the provider. |
| `expected-transition-missing` | The runtime never invoked the tool through MCP, or the state machine stopped early. |
| `unexpected-transition` | A request arrived after completion, or a conversation restarted without the issued calls. |
| `tool-arguments-changed` | The arguments that reached MCP differ from the ones the provider issued. |
| `evidence-inconsistent` | The MCP evidence log does not match the deterministic payload generator (tampered or corrupted evidence). |

A proven failure takes precedence over an unknown in the same scenario.

## Runtime adapters

An adapter is a JSON manifest that tells the harness how to configure, start, drive and stop a runtime. The comparison engine never sees it.

```json
{
  "name": "example-python-agent",
  "description": "free text",
  "provider_protocol": "openai-chat-completions",
  "capabilities": ["history-replay", "persistence-resume"],
  "environment": {
    "OPENAI_BASE_URL": "{{provider_base_url}}",
    "OPENAI_API_KEY": "boundarycheck-test-key",
    "BOUNDARYCHECK_MCP_COMMAND": "{{mcp_command}}",
    "BOUNDARYCHECK_MCP_ARGS": "{{mcp_args_json}}",
    "BOUNDARYCHECK_PROMPT": "{{scenario_prompt}}",
    "BOUNDARYCHECK_RUNTIME_INFO": "{{runtime_info_path}}",
    "BOUNDARYCHECK_STATE_PATH": "{{workdir}}/runtime-state.json"
  },
  "resume": {
    "environment": { "BOUNDARYCHECK_RESUME": "1" },
    "args": [],
    "stdin": null
  },
  "args": [],
  "files": [{ "path": "mcp.json", "content": "{\"command\": {{mcp_command_json}}, \"args\": {{mcp_args_json}}}" }],
  "stdin": null,
  "readiness":  { "type": "provider-request", "timeout_seconds": 30 },
  "completion": { "type": "provider-scenario-complete", "exit_grace_seconds": 5 },
  "shutdown":   { "grace_seconds": 3 },
  "runtime_version": { "type": "report-file" }
}
```

| Contract item | Manifest field |
|---|---|
| 1. Provider protocol | `provider_protocol`. Must be `openai-chat-completions`. |
| 2. Provider base URL | Any `environment`/`args`/`files`/`stdin` template using `{{provider_base_url}}`. |
| 3. Dummy API key | A literal value in `environment`. The parent environment's `OPENAI_*` variables are never inherited. |
| 4. MCP stdio registration | `{{mcp_command}}` and `{{mcp_args_json}}` (a JSON array) in env vars, args, or a generated config file (`files`, written into the scenario work directory). |
| 5. Initial user turn | `{{scenario_prompt}}` through env, args, a file, or `stdin`. |
| 6. Readiness | `provider-request`: the runtime is ready when its first provider request arrives. |
| 7. Completion | `provider-scenario-complete`: done when the final answer is sent; the runtime then has `exit_grace_seconds` to exit. `process-exit`: also waits for the runtime to exit with code 0. |
| 8. Termination | SIGTERM to the runtime's process group, then SIGKILL after `shutdown.grace_seconds`. Stray group members, such as a leaked MCP server, are killed too. |
| 9. Runtime version | `static` (`value`), `command` (`argv`, whose stdout is the version), `report-file` (the runtime writes `{"name","version",...}` to `{{runtime_info_path}}`), or `unknown`. |
| 10. Replay/resume | Declare `history-replay` and/or `persistence-resume` in `capabilities`. The latter also requires a `resume` block whose environment, args and optional stdin are applied to the second process. |

**Templates.**

- Variables: `provider_base_url`, `mcp_command`, `mcp_args_json`, `scenario_prompt`, `scenario_id`, `run_id`, `workdir`, `runtime_info_path`.
- `{{name_json}}` inserts any variable as a JSON string literal.
- An unknown variable is a configuration error, reported before anything is launched.

**Environment.**

- `inherit_environment: "minimal"` (the default) passes only `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `TZ`, `TERM`, plus any exact names listed in `environment_passthrough` (for example `VIRTUAL_ENV`). Your shell's API keys and tokens never reach the runtime.
- `inherit_environment: "all"` passes everything except `OPENAI_*`, `BOUNDARYCHECK_*` and any variable whose name looks like a credential (contains key, token, secret, auth, session, password or credential), unless it is listed in `environment_passthrough`.
- The adapter's variables are then applied.
- `NO_PROXY` / `no_proxy` gain `127.0.0.1,localhost`.

Built-in adapters:

- `fixture-agent`: the stdlib-only Python fixtures in `fixtures/`.
- `openai-agents-python`: the OpenAI Agents SDK through `adapters/openai-agents-python/agent.py`. This shim only wires `OpenAIChatCompletionsModel`, `MCPServerStdio`, and the SDK's native [`SQLiteSession`](https://openai.github.io/openai-agents-python/sessions/) to the fake boundaries. The SDK handles tool results and persisted history itself. Pass `--stream` to the shim to use `Runner.run_streamed`.

## Real runtime result

| | |
|---|---|
| Runtime | `openai-agents` **0.23.1**, with `openai` 3.26.0 and `mcp` 2.3.0 (pinned in `adapters/openai-agents-python/requirements.txt`) |
| Python | 3.13.5 |
| OS | macOS 26.7 (Darwin 25.6.0, arm64) |
| Result | 12/12 PASS, both non-streaming and streaming (`--stream`) |

Observed behavior:

- The SDK sends MCP text results as a one-element array of text parts. The transport representation is `changed`, and the extracted text bytes are `identical`.
- The SDK retried the HTTP 429 with the identical result.
- Two MCP calls were simultaneously in flight and completed B then A without losing their call-ID association.
- `SQLiteSession` preserved the tool result through both same-process replay and a real process exit/restart.

No framework defect was found. Every FAIL in this repository's demo and tests comes from the faulty fixture, whose faults are deliberately injected.

## Fixture runtimes

- `fixtures/conforming-agent/agent.py` forwards results unchanged. With `--reorder`, it reverses concurrent results; this is legal and must still PASS.
- `fixtures/faulty-agent/agent.py --fault NAME[,NAME…]` injects deliberate faults:
  - `truncate`, `head-tail`, `head-tail-marker`, `utf8-split`
  - `swap`, `duplicate`, `missing`, `retry-mutation`, `replay-mutation`
  - `normalize`, `json-float`
  - `json-dup-key` (hides a different value in an earlier duplicate key)
  - `escape` (starts a `setsid()` child, then hangs)
  - `echo-user` (copies each result into an extra user message) and `extra-id` (adds a tool result under an unissued ID)
  - `reserialize-json`, a legal change that must PASS
  - `exit-early` and `hang`, which must give UNKNOWN

## Reports and evidence

The JSON report (`--report`) contains:

- **Run information:**
  - `schema_version` (`boundarycheck.report/v1`) and `boundarycheck_version`
  - `run_id`, `started_at` and `duration_ms`
  - `adapter`, and `runtime` (name, version, version source, details)
  - `provider_protocol`, and `command.argv` (argument vector, with secret-looking values redacted)
  - `platform`
- **Per scenario:**
  - verdict, classifications, findings (proven differences with byte offsets, retained head and tail lengths, inserted-text excerpt, missing sentinels, JSON path), and unknown reasons
  - for each call: tool bytes and hash (content and transport), and each provider occurrence with request, attempt, occurrence count, message indexes, bytes, hash, and the three comparison levels
  - every provider request: sequence number, offsets and timestamps, role, status, size and hash
  - final state-machine state, process exits/restart count, and timing
- **Totals:** `summary` and `exit` (`code` and `classification`).

Artifacts (`--artifacts DIR`) are opt-in:

```text
DIR/BC_RUN_000001/
  run.json
  large-text-100k/
    tool-response-BC_CALL_000001.raw   exact bytes the MCP server wrote
    provider-request-002.raw           exact HTTP body the provider received
    provider-request-002.headers.json  only with --save-headers, redacted
    runtime-stdout.log, runtime-stderr.log
    comparison.json
```

**Privacy:**

- Headers are not stored unless `--save-headers` is given. When they are, `authorization`, cookies, and any header whose name contains key, token, secret, auth, session, password or credential are replaced with `[REDACTED]`.
- The parent environment is never copied into reports.
- Raw artifacts are capped at 8 MiB per file, and runtime stdout and stderr at 1 MiB per stream.
- Files, including the `--report` file, are created `0600`, and directories `0700`.
- Evidence and report files are opened with `O_NOFOLLOW`; symlink targets are refused.
- Request bodies outside the active run are discarded.

## Testing

```bash
cargo test     # 37 unit tests + 14 end-to-end tests (requires python3)
```

The unit tests cover:

- SHA-256
- exact comparison
- canonical JSON and the first changed path
- sentinel generation and detection
- every truncation shape and the invalid-UTF-8 boundary
- tool discovery and the state machine
- header and argv redaction
- template rendering
- verdict-to-exit-code mapping
- process-group termination
- same-process replay and process-restart resume
- concurrent MCP completion in reverse order
- evidence-file symlink refusal

The suite passes on macOS 26.7 (arm64) and on Linux (Debian bookworm in Docker, Python 3.11). `.github/workflows/ci.yml` runs formatting, clippy and the tests on both.

The integration tests launch the real binary, the provider, the MCP server and the Python fixture runtimes as processes, and assert on the final JSON report. They cover:

- the conforming pass and legal reordering
- every injected fault
- legal JSON re-serialization
- early exit
- timeout, with process cleanup verified by PID
- configuration errors
- a missing tool
- `content: null` tool results
- a result copied into a non-tool message, and a result under an unissued ID
- a hidden duplicate JSON key
- a `setsid()` child that leaves the process group
- Ctrl-C: children killed, work directory removed, exit code 2
- persisted replay after a real process restart, including deliberate replay-only mutation
- report-file symlink refusal
- artifact layout, permissions and redaction

## Known limitations

- Only OpenAI Chat Completions is supported: no Responses API and no other providers.
- Only MCP over stdio is supported.
- Only text tool results are covered: no image or resource content.
- Error status is not observable with Chat Completions, so only error content is checked.
- The runtime is assumed not to be adversarial. It runs as the same OS user and launches the MCP server itself, so no file, socket or secret can be kept out of its reach. boundarycheck cross-checks the evidence log against its payload generator, which catches corrupted or edited evidence; it cannot stop a runtime written to fool the test. Testing hostile code needs OS-level isolation (a separate user or sandbox), which is outside this tool.
- Replay and persistence/resume require explicit adapter capabilities because runtimes expose different lifecycle APIs. Unsupported lifecycle scenarios are omitted by default and rejected if selected explicitly.
- One runtime process is started per scenario. Runtimes with expensive startup make a run slower.
- On macOS, a process that leaves the runtime's process group *and* is orphaned because its parent already exited is reparented to launchd and cannot be found. While its parent is alive it is found and killed, as it always is on Linux, where boundarycheck is a child subreaper. Output capture never waits more than 2 seconds for such a process.
- Tool discovery accepts `boundary_test` or a single `<sep>boundary_test` suffix. Runtimes that rename tools in any other way are reported as `adapter-insufficient`.

## Project layout

```text
src/
  main.rs, cli.rs        command line
  runner.rs              per-scenario orchestration, timeouts, artifacts
  adapter.rs             adapter manifests and templates
  process.rs             process groups, bounded capture, cleanup
  report.rs              JSON schema and human output
  scenario.rs            scenario registry (shared by provider and MCP server)
  compare/               content, canonical JSON, classification, evaluation
  provider/              Chat Completions protocol, state machine, HTTP server, recorder
  mcp/                   stdio MCP server, deterministic payloads
  model/                 observations and verdicts
adapters/                built-in adapter manifests and the Agents SDK shim
fixtures/                conforming and faulty fixture runtimes
tests/integration.rs     end-to-end tests
scripts/demo.sh          reproducible demonstration
```
