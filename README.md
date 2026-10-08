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

## Installation

From source (Rust 1.80 or later; boundarycheck is not on crates.io):

```bash
cargo install --git https://github.com/kraftaa/boundcheck --locked boundarycheck
```

Prebuilt binaries for Linux (x86_64, aarch64) and macOS (Apple silicon, Intel) are attached to each [GitHub release](https://github.com/kraftaa/boundcheck/releases), together with `SHA256SUMS` and a build-provenance attestation. To verify a download:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify boundarycheck-v0.1.0-aarch64-apple-darwin.tar.gz --repo kraftaa/boundcheck
```

Releases are built by `.github/workflows/release.yml` when a `v*` tag matching the `Cargo.toml` version is pushed. Vulnerabilities: see [SECURITY.md](SECURITY.md).

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
VIRTUAL_ENV=.venv-agents uv pip install --require-hashes --no-deps -r adapters/openai-agents-python/requirements.lock
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
| `--scenario <name>` | Run only this scenario. Repeatable. By default every scenario supported by the selected adapter runs. Also accepts `large-text:<bytes>` for any size from 1,024 bytes to 8 MiB, with an optional `k` (KiB) or `m` (MiB) suffix, for example `large-text:65537` or `large-text:2m`. |
| `--extended` | Also run the extended sizes: 65,535 / 65,536 / 65,537 bytes, 250 KiB, 1 MiB − 1 / 1 MiB / 1 MiB + 1, and 5 MiB. Cannot be combined with `--scenario`. |
| `--probes` | Also run the probe scenarios: images, audio and resources, which the provider protocol cannot carry in a tool result. Their best verdict is UNKNOWN, and the report says what the runtime did with each block. Cannot be combined with `--scenario`. |
| `--keep-workdir` | Keep the temporary work directory and print its path. |
| `--save-headers` | Save provider request headers with the artifacts. Credentials are always redacted. |
| `--isolation docker` | Run boundarycheck and the runtime inside a locked-down container (see [Isolation](#isolation)). Needs `--image`. |
| `--image <image>` | Container image for `--isolation docker`. It must contain the runtime and its dependencies. |
| `--isolation-binary <path>` | A Linux build of boundarycheck to run inside the container. Defaults to the running executable, which only works on Linux; on macOS, build one with `scripts/build-linux-binary.sh`. |
| `--container-engine <cli>` | `docker` (default), `podman`, `nerdctl`, ... |

boundarycheck runs the agent command directly, without a shell. Ports are allocated dynamically on `127.0.0.1` and are never exposed as flags. Each scenario starts a fresh runtime process; `persistence-resume` deliberately starts a second process from persisted state.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Every selected scenario passed. |
| 1 | At least one scenario failed. |
| 2 | Harness, adapter or configuration error, for example an unknown adapter, a command that cannot be launched, or a provider that cannot bind. Also returned on Ctrl-C. |
| 3 | No failures, but at least one scenario was unknown. |

## Provider protocols

boundarycheck implements two provider protocols. An adapter declares one with `provider_protocol`, and any other value is rejected with exit code 2.

| `provider_protocol` | Endpoint | Tool results can carry |
|---|---|---|
| `openai-chat-completions` | `POST {base}/chat/completions` | text |
| `openai-responses` | `POST {base}/responses` | text and images |

Each protocol has its own request parser and response builders (`src/provider/protocol.rs`, `src/provider/responses.rs`). Both map requests onto one protocol-neutral model, so the comparison engine is shared. The Chat Completions details come first below; the [Responses API](#openai-responses-api) follows.

### OpenAI Chat Completions

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

**Retry scenario.** The first tool-result request gets HTTP 429 with `retry-after-ms: 50` and `retry-after: 1` headers and an `error.type` of `rate_limit_error`. The other transport faults are described in [Transport faults](#transport-faults).

**Error status.** Chat Completions tool messages have no error flag. An MCP `isError: true` result can therefore only be checked by its content. The report shows `error_status: "not-representable"`.

### OpenAI Responses API

**Supported request shape.** `input` is a string, or an array of items:

- messages: `{"role", "content"}`, with or without `"type": "message"`. `content` is a string or a list of `input_text`/`output_text` and `input_image` parts.
- `{"type": "function_call", "call_id", "name", "arguments"}`, the runtime's echo of a call.
- `{"type": "function_call_output", "call_id", "output"}`. `output` is a string or a list of `input_text`, `input_image` and `input_file` items. Text items are concatenated in order. An `input_image` with a `data:` URL is compared by the SHA-256 of its decoded bytes.
- other item types, such as `reasoning`, are kept as opaque entries.

Tools are read from `tools[]` entries with `"type": "function"` and a `name`. `model`, `stream` and `parallel_tool_calls` are read.

**`previous_response_id`.** The fake provider stores the conversation of every response it sends: the input items followed by its output items. A request that names a `previous_response_id` is evaluated as the stored conversation followed by its own `input`, which is exactly what the model would see. So a runtime that relies on server-side history is checked as strictly as one that resends everything. An unknown `previous_response_id` gets HTTP 400 and the scenario becomes UNKNOWN.

**Responses.** Responses are deterministic `response` objects (`resp_boundarycheck_NNNNNN`) with `function_call` or `message` output items. When `stream` is true, the stream is the event sequence `response.created`, `response.in_progress`, `response.output_item.added`, then `response.function_call_arguments.delta`/`.done` or `response.content_part.added`/`response.output_text.delta`/`.done`/`response.content_part.done`, then `response.output_item.done` and `response.completed`, with `sequence_number`s. The 429 retry and the replay control answers work as for Chat Completions.

**Error status.** As in Chat Completions, a `function_call_output` has no error flag, so only error content is checked.

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
| `large-text:<bytes>` | The same payload at any size (1 KiB to 8 MiB). Sizes just around round numbers show a cutoff exactly: a runtime that keeps 65,536 bytes passes `large-text:65536` and fails `large-text:65537`. `--extended` runs a preset list. |
| `concurrent-two-tools` | Two calls are put in flight together: `BC_CALL_000001` → `city=Boston`, `BC_CALL_000002` → `city=Chicago`. Call A is delayed so B completes first; results must remain associated by ID. |
| `sequential-history` | Two calls in consecutive turns. Every later request must still contain each earlier result exactly once and unchanged. |
| `structured-json` | Nested JSON with `9007199254740993`, `12345678901234567890`, `19.990`, `1e-7`, `-0.0`, unicode keys and deep nesting. Compared semantically. |
| `retry-429` | The result request gets HTTP 429 once. The retried request must carry the same result. |
| `retry-500` / `retry-503` | The same with HTTP 500 / 503. |
| `retry-repeated` | HTTP 429 twice in a row; every retry must carry the same result. |
| `disconnect-after-request` | The provider reads the whole result request, then closes the connection without answering. |
| `connection-reset` | The provider reads the whole result request, then resets the TCP connection (RST). |
| `truncated-response` | The response to the result request is cut off halfway (JSON body or SSE stream). |
| `slow-response` | The response arrives after 3 seconds; there must be no duplicate or changed result. |
| `mcp-error` | An `isError: true` result. Its content must arrive unchanged. |
| `unicode-boundaries` | NFC/NFD pairs, stacked and leading combining marks, ZWJ emoji, flags, VS16, RTL, Devanagari, Hangul jamo, astral characters, a BOM, ZWNJ, fullwidth characters and ligatures. Byte-exact. |
| `replay-history` | The completed tool-result history is submitted a second time by the same process and must remain unchanged. Requires adapter capability `history-replay`. |
| `persistence-resume` | The runtime persists its completed history, exits, and a new process reloads and submits it. Requires adapter capability `persistence-resume`. |
| `multi-text-blocks` | Three MCP text blocks. They must arrive unchanged and in order (see [MCP content](#mcp-content)). |
| `structured-content` | A text summary plus `structuredContent`. Either one must arrive unchanged. |
| `error-with-metadata` | An `isError` result with `structuredContent` and `_meta`. The error text must arrive unchanged. |
| `mcp-protocol-error` | A JSON-RPC error instead of a result. Its exact message must reach the provider. |
| `mixed-content` *(probe)* | Text, a PNG image, text. |
| `embedded-resource` *(probe)* | Text plus an embedded text resource and an embedded blob resource. |
| `resource-link` *(probe)* | Text plus a `resource_link`. |
| `audio-content` *(probe)* | Text plus an audio block. |

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

## Transport faults

The fake provider injects deterministic failures on the first request(s) that carry tool results. The request is always read and recorded in full first, so its content is checked like any other.

| Fault | On the wire |
|---|---|
| HTTP status (`retry-429`, `retry-500`, `retry-503`, `retry-repeated`) | An error JSON body with `retry-after-ms: 50` and `retry-after: 1`. |
| `disconnect-after-request` | The socket is closed without a response. |
| `connection-reset` | `SO_LINGER` 0, then close: the client receives a TCP RST. |
| `truncated-response` | The full `content-length` header, then only half of the body (a final answer as JSON or as an SSE stream), then close. |
| `slow-response` | A normal answer after 3 seconds. |

If the runtime retries, every retried request must carry the unchanged results. A change on the retry is `RetryMutation`, and the finding's `underlying_class` gives the cause. If the runtime gives up and exits, the scenario is UNKNOWN (`runtime-exited-early`), because no transfer fault is proven. These faults need byte-level control of the connection, so the fake provider uses its own small HTTP/1.1 server (`src/provider/server.rs`). It handles keep-alive, `Content-Length` and `chunked` request bodies, and `Expect: 100-continue`.

## MCP content

An MCP result is an ordered list of blocks (`text`, `image`, `audio`, `resource_link`, `resource`), plus optional `structuredContent`, `_meta` and `isError`. Instead of a result, the server can also answer with a JSON-RPC error. A provider protocol may be unable to carry some block types in a tool result. The rules below separate a faithful translation from a lossy one; a required transformation is never reported as corruption.

| MCP content | A faithful copy | Otherwise |
|---|---|---|
| One or more text blocks | The blocks, in order and byte-identical: one text part per block, or one string with the blocks joined by `""`, `"\n"` or `"\n\n"`. A non-empty separator is reported as a note. | FAIL with the usual classification (truncation, mutation, ...). |
| `structuredContent` | Either the text blocks (above) or the structured value as JSON, compared semantically and without duplicate keys. The choice is reported as a note. | `StructuralMutation` with the first changed JSON path, if the runtime sent JSON. |
| JSON-RPC error | Text that contains the exact error message, alone or inside the runtime's own wording. | `ContentMutation`: the message never reached the provider. |
| `image`, `audio`, `resource_link`, `resource` | Not representable in a Chat Completions tool result. | UNKNOWN (`unrepresentable-content`) if every text block still arrived unchanged and in order. The detail says, for each block, whether it was *carried inside the tool content*, *carried elsewhere in the request*, or *dropped*. FAIL if a text block was lost or changed. |

The protocol-neutral model is `src/model/content.rs`. `Representable` declares which block types a provider protocol can carry. It is the extension point for protocols with richer tool results, such as the Responses API, where images are representable and the image scenarios become normal PASS/FAIL scenarios.

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
| `RetryMutation` | The retried request differs from the MCP result, although attempt 1 matched it. The finding's `underlying_class` keeps the proven cause (for example `ContentMutation`). |
| `ContentMutation` | Any other proven difference: Unicode normalization (NFC/NFD/NFKC/NFKD), inserted text, or replaced text. |
| `ReplayMutation` | A result that arrived intact originally is missing, duplicated, reassociated or changed in replayed or restored history. The finding's `underlying_class` keeps the cause (`MissingResult`, `DuplicateResult`, `WrongToolCallAssociation`, …); each scenario also lists `underlying_classifications`. The human output shows `ReplayMutation (underlying: MissingResult)`. |
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
| `unrepresentable-content` | The MCP result contains blocks the provider protocol cannot carry in a tool result; the text blocks arrived unchanged. |

A proven failure takes precedence over an unknown in the same scenario.

## Isolation

`--isolation docker` re-runs boundarycheck itself inside a container. The fake provider, the MCP server and the runtime all run there and talk over the container's own loopback, so the container gets **no network at all**.

| | Inside the container |
|---|---|
| Network | `--network none`: no internet, no DNS, no access to host services. The runtime cannot reach real model APIs or send data anywhere. |
| Files | Only the current directory, mounted **read-only** at `/work`, plus the `--report` and `--artifacts` directories (read-write). Your home directory, SSH keys and cloud credentials are not visible. |
| Filesystem | `--read-only` root and a writable in-memory `/tmp` (the work directory). |
| Privileges | `--cap-drop ALL`, `no-new-privileges`, `--pids-limit 512`, running as your UID so output files belong to you. |
| Environment | Only what the adapter sets, plus `HOME=/tmp`. Your shell's variables are not passed in. |

```bash
scripts/build-linux-binary.sh            # macOS only: a Linux build of boundarycheck
docker build -t boundarycheck-openai-agents -f adapters/openai-agents-python/Dockerfile .
boundarycheck run --adapter openai-agents-python --isolation docker \
  --image boundarycheck-openai-agents --isolation-binary target/linux-arm64/release/boundarycheck \
  --report out/report.json -- python3 adapters/openai-agents-python/agent.py
```

The image must contain everything the runtime needs, because nothing can be downloaded at run time. `adapters/openai-agents-python/Dockerfile` installs the Agents SDK from the hash-locked requirements. Paths in the agent command are relative to the current directory (`/work` inside). An adapter manifest given as a path must be inside the current directory. The report records `isolation: "docker image=... network=none"`.

Isolation protects **your machine** from the runtime under test. It does not make the test tamper-proof: the runtime still runs next to the harness inside the container (see [SECURITY.md](SECURITY.md)). The CI `isolation` job checks the guarantees end to end: a probe "agent" inside the container finds no network, no host home directory and no inherited secret.

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
- `fixture-agent-responses`: the same fixtures speaking the Responses API. The conforming fixture's `--previous-response-id` flag sends only new items after the first response.
- `openai-agents-python-responses`: the Agents SDK with `OpenAIResponsesModel` (the shim's `--responses` flag).
- `openai-agents-python`: the OpenAI Agents SDK through `adapters/openai-agents-python/agent.py`. This shim only wires `OpenAIChatCompletionsModel`, `MCPServerStdio`, and the SDK's native [`SQLiteSession`](https://openai.github.io/openai-agents-python/sessions/) to the fake boundaries. The SDK handles tool results and persisted history itself. Pass `--stream` to the shim to use `Runner.run_streamed`.

## Real runtime results

Three runtimes are tested, each over both protocols and with and without streaming: the OpenAI Agents SDK, Pydantic AI and LangGraph (with `langchain-mcp-adapters`). The full, generated table with an explanation for every non-PASS result is in **[docs/compatibility.md](docs/compatibility.md)**. Regenerate it, and the baselines CI checks against, with `scripts/compat_matrix.py --write-baselines`.

### OpenAI Agents SDK in detail

| | |
|---|---|
| Runtime | `openai-agents` **0.23.1**, with `openai` 3.26.0 and `mcp` 2.3.0 (pinned in `adapters/openai-agents-python/requirements.txt`; the full dependency set is hash-locked in `requirements.lock`) |
| Python | 3.13.5 |
| OS | macOS 26.7 (Darwin 25.6.0, arm64) |
| Chat Completions | 15/16 PASS and 1 FAIL on the default suite; the 8 extended sizes PASS; the 4 probes are UNKNOWN. Identical in non-streaming and streaming (`--stream`) mode. Baseline: `expected.json`. |
| Responses API | Same default-suite and extended results; of the probes, `mixed-content` PASSes (the image arrives as an `input_image` with identical bytes), and the other 3 are UNKNOWN. Identical with `--stream`. Baseline: `expected-responses.json`. |
| Baselines | In `adapters/openai-agents-python/`. CI compares every run with them via `scripts/check_expected.py`, so a change in either direction is noticed. |

Observed behavior:

- The SDK sends MCP text results as a one-element array of text parts. The transport representation is `changed`, and the extracted text bytes are `identical`.
- The SDK retried the HTTP 429 with the identical result.
- Two MCP calls were simultaneously in flight and completed B then A without losing their call-ID association.
- `SQLiteSession` preserved the tool result through both same-process replay and a real process exit/restart.
- Several text blocks are sent as one text part per block; `structuredContent` is not used by default (`use_structured_content=False`), so the text summary is sent.
- Transport faults: HTTP 429/500/503 (including two 429s in a row), a closed connection, a TCP reset and a slow response are retried with unchanged results, in both protocols and both modes. A response body cut off halfway is retried in non-streaming mode. **In streaming mode it is not:** the `openai` client raises `APIConnectionError: Connection error.` while reading the stream, and its automatic retries only cover a request that fails before the response starts, so the run ends. `truncated-response` is therefore UNKNOWN with `--stream`, and recorded that way in both baselines.
- **`mcp-protocol-error` FAILs because of a documented SDK default, not a defect.** When the MCP server answers with a JSON-RPC error, the SDK replaces the message with `"An error occurred while running the tool. Please try again."` (`agents/tool.py`, `default_tool_error_function`, whose docstring reads: *"Return a fixed error response without exposing the exception to the model. Provide a custom `failure_error_function` to return application-approved error details."*). The adapter tests the SDK's defaults, so it does not override this. A tool-level error (`isError: true`, scenario `mcp-error`) is passed through unchanged.
- Probes over Chat Completions: the image block is dropped from the request. Over Responses, the image is sent as an `input_image` part with identical bytes. In both, audio, `resource_link` and embedded resources are serialized as JSON inside the tool text.

Every other FAIL in this repository's demo and tests comes from the faulty fixture, whose faults are deliberately injected.

## Fixture runtimes

- `fixtures/conforming-agent/agent.py` forwards results unchanged. With `--reorder`, it reverses concurrent results; this is legal and must still PASS.
- `fixtures/faulty-agent/agent.py --fault NAME[,NAME…]` injects deliberate faults:
  - `truncate`, `truncate-64k`, `head-tail`, `head-tail-marker`, `utf8-split`
  - (the fixture runtimes retry HTTP 429/500/502/503, closed or reset connections and truncated bodies, up to 4 attempts)
  - `replay-drop` (drops tool results from replayed history)
  - `swap`, `duplicate`, `missing`, `retry-mutation`, `replay-mutation`
  - `normalize`, `json-float`
  - `json-dup-key` (hides a different value in an earlier duplicate key)
  - `escape` (starts a `setsid()` child, then hangs)
  - `echo-user` (copies each result into an extra user message) and `extra-id` (adds a tool result under an unissued ID)
  - `join-space`, `drop-block`, `structured-mutate`, `error-generic` (content-model faults)
  - `reserialize-json`, `join-newline` and `send-structured`, legal changes that must PASS
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
cargo test     # 48 unit + 22 end-to-end + 14 property tests (requires python3; the Docker isolation test runs only when BOUNDARYCHECK_DOCKER_IMAGE is set)
```

**False-PASS properties** (`tests/false_pass.rs`, run by `cargo test`). A simulated runtime drives the real provider state machine and the real MCP server. On every run it applies a random transformation to the tool results, either when they are first delivered, on the 429 retry, or in the replayed history:

- **Legal** transformations must PASS: splitting content into text parts, `\uXXXX` escaping, reordering, and pretty-printing JSON.
- **Illegal** ones must never PASS: truncation, appended or replaced characters, dropping, duplication, swapped or changed IDs, a missing ID, NFC normalization, empty or `null` content, copying into a user message, and an extra result under an unissued ID.

An oracle written independently of `compare/` checks every PASS. Other properties check that the classifier reports "no difference" exactly when the bytes are equal, that JSON equality is symmetric and agrees with the canonical form, that number comparison is exact (including exponents beyond `i128`), that duplicate keys are always found, and that the parsers never panic. The suite was mutation-tested: disabling one evaluator check makes it report a false PASS with a minimal reproduction.

**Fuzzing** (`fuzz/`, nightly Rust and `cargo-fuzz`) has five targets: `provider_request` (raw request bodies through both protocol parsers, the state machine and the evaluator), `http_request` (raw bytes on the fake provider's socket: request line, headers, `Content-Length` and chunked bodies, pipelining), `mcp_server` (arbitrary stdin; every output line must be JSON-RPC), `classify_text` and `json_compare`.

```bash
cd fuzz && cargo +nightly fuzz run provider_request -- -max_total_time=300
```

Runs so far found no crash or failed check: 90 seconds per target (about 7.2 million inputs), then 2 minutes each for the extended `provider_request` (713,000 inputs) and the new `http_request` (1.6 million inputs). `.github/workflows/fuzz.yml` runs all five weekly and on demand, and keeps any crash reproducer as an artifact.

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
- the underlying cause of retry and replay findings
- a 65,536-byte cutoff isolated by the threshold sizes
- Ctrl-C: children killed, work directory removed, exit code 2
- persisted replay after a real process restart, including deliberate replay-only mutation
- report-file symlink refusal
- artifact layout, permissions and redaction

## Known limitations

- Two provider protocols are supported: OpenAI Chat Completions and the OpenAI Responses API. Other providers are not. For Responses, only the function-calling surface above is implemented: no hosted tools, background mode or Conversations API.
- Only MCP over stdio is supported.
- Images, audio and resources cannot be carried in a Chat Completions tool result, so those scenarios are opt-in probes whose best verdict is UNKNOWN.
- Error status is not observable with Chat Completions, so only error content is checked.
- The runtime is assumed not to be adversarial. It runs as the same OS user and launches the MCP server itself, so no file, socket or secret can be kept out of its reach. boundarycheck cross-checks the evidence log against its payload generator, which catches corrupted or edited evidence; it cannot stop a runtime written to fool the test. `--isolation docker` keeps such a runtime away from your files, credentials and network, but not away from the harness itself.
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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
