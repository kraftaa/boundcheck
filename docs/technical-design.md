# Technical design and guarantees

boundarycheck compares independently recorded evidence at the two boundaries around an agent runtime:

```text
deterministic MCP response -> runtime under test -> provider request body
```

No real model is called, runtime tracing is not trusted as ground truth, and no model classifies the result.

## Provider protocols

boundarycheck implements OpenAI Chat Completions and the OpenAI Responses API. Both map onto a protocol-neutral observation model, so correlation and comparison use the same engine.

The adapter receives a scenario-specific base URL:

```text
http://127.0.0.1:<port>/<run_id>/<scenario_id>/v1
```

Supported endpoints include:

- `POST {base}/chat/completions`
- `POST {base}/responses`
- `GET {base}/models`

Other paths return HTTP 404. Paths for a different run or scenario return HTTP 403 and their bodies are discarded.

The provider reads the offered function tool, message roles or Responses input items, call IDs, content, streaming mode, and relevant options. Responses use deterministic IDs, arguments, sequence numbers, timestamps, and final text.

Responses API requests may use `previous_response_id`; the fake provider reconstructs the stored conversation before evaluating it. Images carried as data URLs are compared by the SHA-256 of their decoded bytes.

## MCP boundary

The hidden `boundarycheck mcp-server` command implements newline-delimited JSON-RPC 2.0 over stdio. It supports `initialize`, `ping`, `tools/list`, `tools/call`, plus empty resource and prompt lists.

Its single `boundary_test` tool receives `{run_id, scenario, call_id}`. Payloads contain no clocks or randomness. Results may contain ordered text, image, audio, resource-link, and embedded-resource blocks, plus structured content, metadata, and error state. The server can also return a JSON-RPC error. Before writing a response to stdout, it records the exact bytes in an evidence log under an exclusive lock.

The harness independently regenerates the expected deterministic payload. An inconsistent or tampered evidence log produces `UNKNOWN` with `evidence-inconsistent`.

## Comparison model

Evidence is evaluated at three levels:

1. **Transport:** raw MCP response bytes and raw provider request bytes, including JSON representation.
2. **Extracted content:** logical text bytes, compared byte-for-byte and by SHA-256.
3. **Semantic value:** for structured JSON, exact canonical comparison of objects, strings, and decimal numbers.

JSON object order does not matter. Numbers compare by exact decimal value, not binary floating point. Duplicate keys are rejected as structural mutations. Strings are not Unicode-normalized.

## Correlation rules

- Provider `tool_call_id` is the primary key; content similarity is never the primary matcher.
- Every request after a tool call must contain exactly one unchanged tool message for each issued call.
- Result order does not matter.
- Exact content under the wrong ID is `WrongToolCallAssociation`.
- Repeated content is `DuplicateResult`.
- Unissued or missing IDs without decisive evidence produce `UNKNOWN`.

Each payload also includes its `run_id`, scenario, and call ID, providing a second independent association signal.

## Verdicts and classifications

`FAIL` is emitted only for a proven difference. Main classifications include:

| Classification | Proven condition |
|---|---|
| `MissingResult` | An issued call has no corresponding result in a later request. |
| `DuplicateResult` | A result is repeated or copied into another message. |
| `WrongToolCallAssociation` | Exact result content is attached to the wrong or missing call ID. |
| `Truncation` | Content is a provable prefix, suffix, head/tail, interior slice, or empty value. |
| `InvalidUtf8` | Invalid UTF-8 or a replacement character appears at the changed boundary. |
| `InvalidJson` | Structured content no longer parses. |
| `StructuralMutation` | Canonical JSON differs or contains duplicate keys. |
| `RetryMutation` | Content changes specifically on the retried request. |
| `ReplayMutation` | Previously intact content changes, disappears, or moves during replay/restoration. |
| `ContentMutation` | Another byte-level change is proven. |

`UNKNOWN` covers unsupported request shapes, ambiguous correlation, early exit, timeouts, unparseable requests, insufficient adapters, unexpected state transitions, changed tool arguments, and inconsistent evidence.

A proven failure takes precedence over an unknown condition in the same scenario.

## Transport faults

The fake HTTP/1.1 provider can return HTTP 429, 500, or 503; repeat a 429; close or reset the connection; truncate a response body; or delay a response. It records each request completely before injecting the fault. If a runtime retries, every occurrence must contain the unchanged tool result.

The server handles keep-alive, `Content-Length`, chunked request bodies, pipelining, and `Expect: 100-continue` directly so connection-level behavior remains deterministic.

## Determinism

- Run ID: `BC_RUN_000001`, or the next exclusive artifact-directory number.
- Call IDs: `BC_CALL_000001`, `BC_CALL_000002`.
- Provider response IDs, timestamps, arguments, usage, and final text are fixed.
- Scenarios follow a finite state machine; unexpected transitions abort the scenario as `UNKNOWN`.

## Security model

boundarycheck is intended for controlled test agents, not hostile code.

Protections include:

- minimal environment inheritance by default;
- credential-name and common-token redaction;
- opt-in request headers with sensitive values redacted;
- owner-only artifact and report permissions;
- component-by-component `openat`/`O_NOFOLLOW` checks for evidence, report, and artifact paths;
- runtime stdout/stderr excluded from reports and artifacts unless explicitly requested;
- bounded requests, artifacts, and captured output;
- process-group and descendant cleanup on timeout or interruption;
- evidence cross-checking against the deterministic payload generator.

`--isolation docker` additionally removes network access, mounts the project read-only, uses a read-only root filesystem and in-memory temporary directory, drops Linux capabilities, limits processes, and exposes only output directories as writable.

The runtime executes as the same operating-system user and launches the MCP server itself. A runtime deliberately written to fool or attack the harness therefore needs OS-level isolation, such as a sandbox or separate user. That isolation is outside boundarycheck's scope.

## MCP content policy

boundarycheck compares the ordered MCP content blocks emitted by the tool with
the provider-visible function result. Exact text and JSON semantics must be
preserved. Legal, lossless protocol encodings are reported as notes; proven
loss, mutation, duplication, or incorrect call association is `FAIL`. When a
provider protocol cannot represent a block, the verdict is `UNKNOWN` rather
than a false pass or failure.

## Known limitations

- OpenAI Chat Completions and the OpenAI Responses API are supported; other providers are not.
- Only MCP over stdio is supported.
- Some MCP blocks cannot be represented by Chat Completions. Those probes return `UNKNOWN` when text remains intact; the Responses API can carry images.
- Provider function-result formats cannot represent the MCP error flag directly, so error content is checked.
- Replay and persistence tests require explicit runtime support.
- A fresh process per scenario may be slow for expensive runtimes.
- On macOS, an already-orphaned process that escaped its process group can be reparented beyond the harness's process tree. Linux subreaper support closes this gap there.
