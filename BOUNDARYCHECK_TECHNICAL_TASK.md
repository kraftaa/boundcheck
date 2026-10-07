# Boundarycheck — Technical Implementation Task

## Objective

Build `boundarycheck`, a deterministic command-line test harness that verifies whether an AI agent runtime faithfully transfers tool results from an MCP tool boundary to a model-provider boundary.

The harness must independently observe both sides of the runtime under test:

```text
Fake MCP tool server
        ↓
Agent runtime under test
        ↓
Fake model-provider endpoint
```

It must detect missing, duplicated, truncated, mutated, or incorrectly associated tool results without calling a real model and without treating runtime tracing as ground truth.

## Product Principle

The product should answer one question with reproducible evidence:

> The tool produced X. What did the runtime send to the model provider?

The result for each scenario must be `PASS`, `FAIL`, or `UNKNOWN`. If the harness cannot prove a failure, it must not infer one.

## V1 Scope

V1 supports:

- macOS and Linux;
- MCP over stdio;
- one explicitly defined OpenAI provider protocol;
- deterministic scenario state machines;
- exact text-content comparison;
- canonical JSON comparison;
- tool-call correlation using deterministic IDs;
- human-readable and JSON reports;
- optional evidence artifacts;
- at least one real runtime integration;
- a deliberately faulty fixture runtime used to prove detection behavior.

Choose exactly one provider protocol for V1:

- OpenAI Chat Completions, or
- OpenAI Responses API.

Do not describe V1 merely as “OpenAI-compatible.” The selected protocol and supported request shapes must be documented precisely.

## Non-Goals

V1 is not:

- an LLM evaluation framework;
- an observability or tracing platform;
- a production proxy;
- a prompt-testing tool;
- a generic MCP debugger;
- a security scanner;
- a framework benchmark;
- a dashboard;
- a multi-provider compatibility suite.

The harness must not use an LLM to classify or explain failures.

## Command-Line Interface

The target user experience is:

```bash
boundarycheck run --adapter <adapter-name> -- <agent-command>
```

Example:

```bash
boundarycheck run \
  --adapter example-python-agent \
  --report boundarycheck-report.json \
  -- python example_agent.py
```

Recommended initial flags:

```text
--adapter <name>       Required runtime adapter
--report <path>        Write a JSON report
--artifacts <path>     Save failure evidence
--timeout <seconds>    Per-scenario timeout
--scenario <name>      Run one scenario; repeatable
--keep-workdir         Preserve the temporary test directory
```

Avoid exposing provider and MCP ports unless needed for debugging. Prefer dynamically allocated ports.

## Runtime Adapter Contract

An arbitrary agent command cannot be configured reliably without an integration contract. Implement a small adapter interface that tells the harness how to configure, start, drive, and stop a runtime.

An adapter must define:

1. which provider protocol it uses;
2. how the fake provider base URL is injected;
3. how a dummy API key is injected, if required;
4. how the MCP stdio server is registered;
5. how the initial user turn is supplied;
6. how readiness is detected;
7. how successful completion is detected;
8. how the process is terminated and cleaned up;
9. which runtime version is under test.

Suggested manifest shape:

```json
{
  "name": "example-python-agent",
  "provider_protocol": "openai-chat-completions",
  "environment": {
    "OPENAI_BASE_URL": "{{provider_base_url}}",
    "OPENAI_API_KEY": "boundarycheck-test-key",
    "BOUNDARYCHECK_MCP_COMMAND": "{{mcp_command}}",
    "BOUNDARYCHECK_MCP_ARGS": "{{mcp_args_json}}",
    "BOUNDARYCHECK_PROMPT": "{{scenario_prompt}}"
  },
  "readiness": {
    "type": "provider-request"
  },
  "completion": {
    "type": "provider-scenario-complete"
  }
}
```

The exact format may change, but adapter-specific behavior must not leak into the comparison engine.

## Boundary Observations

Capture evidence at three distinct levels. Do not conflate required protocol transformation with corruption.

### 1. Transport Evidence

Record the raw MCP response and raw provider HTTP request body before parsing or normalization.

Transport bytes may legitimately differ because MCP and provider protocols use different envelopes. Raw equality is evidence, but it is not always the verdict.

### 2. Extracted Content

Extract the logical result communicated by each protocol:

- text content;
- structured JSON content;
- error status and error content;
- tool-call identifier;
- result ordering and occurrence count.

For a text-only scenario, compare the UTF-8 text bytes exactly.

### 3. Semantic Value

When both sides contain JSON, parse and compare their canonical semantic values separately from their serialized representation.

Example report:

```text
transport representation: changed
extracted text bytes:     identical
JSON semantics:           identical
```

## Observation Model

Suggested internal types:

```rust
struct ToolObservation {
    run_id: String,
    scenario_id: String,
    tool_call_id: String,
    raw_transport: Vec<u8>,
    extracted_content: Vec<u8>,
    content_sha256: String,
    json: Option<serde_json::Value>,
    is_error: bool,
}

struct ProviderObservation {
    run_id: String,
    scenario_id: String,
    tool_call_id: Option<String>,
    raw_request: Vec<u8>,
    extracted_content: Vec<u8>,
    content_sha256: String,
    json: Option<serde_json::Value>,
    occurrence_index: usize,
}
```

Use the types as guidance rather than a requirement. Keep the initial implementation simple.

## Correlation Rules

Every scenario and payload must contain deterministic identifiers:

```text
run_id       = BC_RUN_000001
scenario_id  = large-text-100k
tool_call_id = BC_CALL_000001
```

Correlation rules:

- use the provider protocol’s tool-call identifier as the primary key;
- include the same identifier inside the generated payload as independent evidence;
- never correlate by fuzzy text similarity;
- distinguish association errors from ordering differences;
- allow concurrent results to arrive in any legal order when their identifiers remain correct;
- report missing or changed identifiers as evidence.

## Deterministic Scenario Engine

Model each scenario as a finite state machine. A scenario controls fake-provider responses and records the expected next event.

Example:

```text
WAIT_INITIAL_REQUEST
  -> SEND_TOOL_CALL
WAIT_TOOL_RESULT
  -> CAPTURE_PROVIDER_REQUEST
  -> SEND_FINAL_RESPONSE
COMPLETE
```

Unexpected transitions must produce `UNKNOWN` or a harness error, not a guessed runtime failure.

## Required MVP Scenarios

### 1. Exact Text

Return a small UTF-8 text result with deterministic identifiers.

Invariant:

```text
extracted MCP text bytes == provider-visible tool-result text bytes
```

Expected result for a conforming runtime: `PASS`.

### 2. Large Text

Test at least 1 KB, 50 KB, and 100 KB payloads. Include unique sentinels near 0%, 25%, 50%, 75%, and 100%.

Detect:

- prefix retention;
- suffix retention;
- head-and-tail retention;
- middle deletion;
- arbitrary truncation;
- inserted replacement or warning text;
- invalid UTF-8 near a truncation boundary.

### 3. Concurrent Tool Calls

The provider requests two calls with different IDs. The MCP server returns deliberately different values:

```text
BC_CALL_A -> Boston
BC_CALL_B -> Chicago
```

Verify that each provider-visible result remains associated with its original call ID. Result order alone must not cause failure.

## Required Post-MVP Scenarios

Add after the end-to-end MVP works:

1. nested structured JSON mutation;
2. duplicate result;
3. missing result;
4. retry with unchanged result;
5. retry with mutated or duplicated result;
6. MCP error result;
7. Unicode and combining-character boundaries;
8. persistence and resume;
9. replay.

## Comparison and Failure Classification

Suggested verdict model:

```rust
enum Verdict {
    Pass,
    Fail(Failure),
    Unknown(UnknownReason),
}
```

Suggested failure classes:

```text
MissingResult
DuplicateResult
WrongToolCallAssociation
ContentMutation
Truncation
InvalidUtf8
InvalidJson
StructuralMutation
RetryMutation
ReplayMutation
ErrorStatusMutation
```

Classifications must be derived from deterministic evidence. When multiple classifications are possible, report the proven facts and use a generic mutation classification.

Define `UNKNOWN` cases explicitly, including:

- unsupported provider request shape;
- ambiguous correlation;
- runtime exits before scenario completion;
- scenario timeout;
- provider request cannot be parsed;
- adapter configuration is insufficient;
- an expected protocol transition never occurs.

## Fake Provider Requirements

The fake provider must:

- implement the selected provider protocol accurately enough for supported runtimes;
- store the raw HTTP request body before parsing;
- preserve request ordering and timestamps;
- return deterministic tool calls;
- inject deterministic retryable failures for retry scenarios;
- expose scenario completion to the runner;
- bind to a dynamically allocated loopback port;
- reject requests that do not belong to the active run where practical.

Request headers should not be stored by default. If diagnostic headers are saved, redact authorization, cookies, and other credentials.

## Fake MCP Server Requirements

The MCP server must:

- use stdio transport;
- implement one tool named `boundary_test`;
- generate payloads deterministically from scenario and call ID;
- record raw response evidence before it is written to stdout;
- keep protocol messages on stdout and diagnostics on stderr;
- support clean termination;
- avoid nondeterministic timestamps or random values inside payloads.

Suggested arguments:

```json
{
  "run_id": "BC_RUN_000001",
  "scenario": "large-text-100k",
  "call_id": "BC_CALL_000001"
}
```

## Process Management

The runner must:

- create an isolated temporary work directory;
- launch the fake provider;
- prepare the MCP stdio command for the adapter;
- launch the runtime command without a shell where possible;
- stream or capture stdout and stderr with bounded memory;
- enforce startup, scenario, and shutdown timeouts;
- terminate child processes on success, failure, cancellation, or panic;
- handle port collisions through dynamic allocation;
- return a harness error when infrastructure fails;
- preserve the temporary directory only when requested or when artifacts are enabled.

## Reports

Human-readable output should be concise:

```text
boundarycheck 0.1.0

PASS exact-text

FAIL large-text-100k
  tool content:      102,400 bytes
  provider content:   50,000 bytes
  first difference:   byte 24,996
  missing sentinels:  middle, 75%
  classification:     head/tail retention

PASS concurrent-two-tools

Summary: 2 passed, 1 failed, 0 unknown
```

The JSON report must include:

- schema version;
- boundarycheck version;
- runtime adapter and runtime version;
- selected provider protocol;
- command in safely represented argument form;
- scenario verdicts;
- content lengths and hashes;
- correlations and occurrence counts;
- proven differences;
- artifact paths, when enabled;
- start time and duration;
- final exit classification.

## Evidence and Privacy

Evidence artifacts are opt-in.

Suggested layout:

```text
.boundarycheck/
  BC_RUN_000001/
    run.json
    exact-text/
      tool-response.raw
      provider-request.raw
      comparison.json
```

Requirements:

- never persist authorization headers by default;
- do not copy the full parent environment into reports;
- bound raw artifact size;
- document that the harness is intended for controlled test agents;
- use restrictive file permissions where supported;
- make artifact paths deterministic within a run.

## Exit Codes

```text
0 = every selected deterministic scenario passed
1 = one or more deterministic scenarios failed
2 = harness, adapter, or configuration error
3 = no failures, but one or more scenarios were unknown
```

## Testing Requirements

### Unit Tests

Cover:

- SHA-256 calculation;
- exact content comparison;
- canonical JSON comparison;
- first changed JSON path;
- sentinel generation and detection;
- truncation classification;
- correlation by call ID;
- duplicate detection;
- missing-result detection;
- association mismatch detection;
- legal concurrent reordering;
- secret redaction;
- verdict-to-exit-code mapping.

### Integration Tests

Build two fixture runtimes:

1. a conforming runtime that passes all supported scenarios;
2. a faulty runtime with selectable faults:
   - truncation;
   - swapped associations;
   - duplication;
   - missing result;
   - retry mutation.

Integration tests must launch the provider, MCP server, and fixture runtime as real processes and assert the final report.

### Real Runtime Demonstration

Integrate at least one maintained agent runtime and record its exact version. A passing result is acceptable for V1.

If a genuine runtime bug is found:

- preserve a minimal reproduction;
- document the exact version and environment;
- distinguish framework behavior from adapter or harness errors;
- never present an intentionally injected failure as a real framework defect.

## Suggested Project Structure

```text
boundarycheck/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── runner.rs
│   ├── adapter.rs
│   ├── process.rs
│   ├── report.rs
│   ├── compare/
│   │   ├── mod.rs
│   │   ├── content.rs
│   │   ├── json.rs
│   │   └── classify.rs
│   ├── provider/
│   │   ├── mod.rs
│   │   ├── server.rs
│   │   ├── protocol.rs
│   │   ├── recorder.rs
│   │   └── scenario.rs
│   ├── mcp/
│   │   ├── mod.rs
│   │   ├── server.rs
│   │   └── payload.rs
│   └── model/
│       ├── observation.rs
│       └── verdict.rs
├── adapters/
├── fixtures/
│   ├── conforming-agent/
│   └── faulty-agent/
└── tests/
```

Prefer a smaller structure if it remains easy to understand.

## Implementation Milestones

### Milestone 1 — End-to-End Proof

- CLI parses an agent command.
- Fake provider captures raw requests.
- MCP stdio server returns an exact-text payload.
- One adapter configures a fixture agent.
- Correlation IDs survive the round trip.
- Exact text produces `PASS` or `FAIL`.

### Milestone 2 — Fault Detection

- Add large-text sentinels.
- Add concurrent calls.
- Add the faulty fixture runtime.
- Produce human and JSON reports.
- Enforce exit codes.

### Milestone 3 — Structured and Retry Cases

- Add semantic JSON comparison.
- Add duplicate and missing-result detection.
- Add deterministic 429 retry scenarios.
- Add evidence artifacts and redaction.

### Milestone 4 — Real Runtime

- Implement one real runtime adapter.
- Pin and report the runtime version.
- Publish a reproducible terminal demonstration.
- Document known limitations.

Persistence and replay are follow-up work and must not delay a reliable live-boundary MVP.

## V1 Acceptance Criteria

V1 is complete when:

- an agent command can be launched through a documented adapter;
- the MCP server records the exact result it emits;
- the provider records the exact request it receives;
- logical tool content is extracted independently at both boundaries;
- deterministic call IDs correlate results without fuzzy matching;
- exact text, large text, concurrent calls, structured JSON, missing, duplicate, and retry scenarios run deterministically;
- legal result reordering does not create a false failure;
- deliberate truncation, mutation, duplication, omission, and association faults are detected;
- a conforming fixture passes;
- a faulty fixture produces the expected failures;
- at least one real runtime is tested and versioned;
- human and JSON reports are produced;
- raw evidence is optional and secret-aware;
- processes are cleaned up on every exit path;
- unit and integration tests pass on macOS and Linux;
- exit status matches the documented verdict rules.

## Definition of Success

The project succeeds when one command can independently demonstrate either:

```text
The test tool produced X.
The runtime sent Y to the provider.
X and Y differ in this exact, reproducible way.
```

or:

```text
The test tool produced X.
The runtime sent the same logical content to the provider.
PASS.
```

No real model, runtime telemetry, or probabilistic evaluator is required.
