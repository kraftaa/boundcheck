# Development and testing

## Standard checks

Python 3 is required by the integration fixtures.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The suite contains unit, end-to-end, and property tests. CI runs formatting, linting, tests, dependency auditing, container-isolation checks, and pinned real-runtime adapters on macOS and Linux.

## Test strategy

Unit tests cover content comparison, exact JSON numbers, duplicate keys, Unicode, sentinels, classifications, provider state transitions, template rendering, credential redaction, reporting, and process cleanup.

Integration tests launch the compiled harness, fake provider, MCP server, and fixture runtime as real processes. They cover conforming behavior, every injected fault, retries, replay and resume, timeouts, Ctrl-C cleanup, symlink refusal, permissions, redaction, and artifact layout.

`tests/false_pass.rs` focuses on the most important invariant: an illegal transformation must never become `PASS`. Legal transformations include JSON escaping, text-part splitting, message reordering, and JSON formatting. Illegal transformations include truncation, insertion, replacement, loss, duplication, changed IDs, Unicode normalization, empty content, copied results, and unissued call IDs.

## Fuzzing

The `fuzz/` package requires nightly Rust and `cargo-fuzz`. Its targets are:

- `provider_request`: both protocol parsers, the state machine, and evaluator;
- `http_request`: raw HTTP request lines, headers, body framing, and pipelining;
- `mcp_server`: arbitrary stdin with JSON-RPC output invariants;
- `classify_text`: text difference classification;
- `json_compare`: canonical JSON comparison.

Run one target for five minutes:

```bash
cd fuzz
cargo +nightly fuzz run provider_request -- -max_total_time=300
```

The scheduled fuzz workflow runs all targets and retains crash reproducers as artifacts.

## Fixture runtimes

`fixtures/conforming-agent/agent.py` forwards tool results unchanged. Its `--reorder` option reverses concurrent completion order, which remains valid.

`fixtures/faulty-agent/agent.py --fault NAME[,NAME...]` injects controlled failures such as truncation, swapped IDs, duplication, loss, retry/replay mutation, Unicode normalization, JSON changes, early exit, hangs, and escaped child processes.

Failures from the faulty fixture demonstrate the harness and are not framework defects.

## Project layout

```text
src/
  main.rs, cli.rs        command line
  runner.rs              scenario orchestration, timeouts, artifacts
  adapter.rs             adapter manifests and templates
  process.rs             process groups, bounded capture, cleanup
  report.rs              JSON schema and human output
  scenario.rs            scenario registry
  compare/               content, canonical JSON, classification, evaluation
  provider/              protocol, state machine, server, recorder
  mcp/                   stdio server and deterministic payloads
  model/                 observations and verdicts
adapters/                built-in manifests and runtime shims
fixtures/                conforming and deliberately faulty runtimes
tests/                   integration and property tests
fuzz/                    cargo-fuzz package and targets
scripts/demo.sh          reproducible demonstration
```

## Compatibility baselines

Pinned runtime adapters keep expected-result files beside their manifests. Regenerate the compatibility table and CI baselines with `scripts/compat_matrix.py --write-baselines`; see [compatibility.md](compatibility.md) for the current results.
