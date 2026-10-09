# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.0] - 2026-10-09

### Added
- Real-runtime compatibility matrix: Pydantic AI 2.54.0 and LangGraph 1.2.14
  (langchain-openai 1.7.0, langchain-mcp-adapters 0.3.2) join the OpenAI
  Agents SDK, each with hash-locked requirements, Chat and Responses
  adapters, baselines and CI jobs. `scripts/compat_matrix.py` runs the
  matrix and generates `docs/compatibility.md` and the baselines.
- Error results may carry runtime wording around their exact content (text,
  or the structured value), and several text blocks may be sent as a JSON
  array of strings; both are reported as notes. Containment requires the
  content's last character to stay intact (no combining mark or joiner
  after it), a false PASS the property tests found in the first version.
- `--isolation docker`: run boundarycheck and the runtime inside a container
  with no network, a read-only root, only the current directory mounted
  read-only, all capabilities dropped and no inherited environment. Adds
  `scripts/build-linux-binary.sh`, an Agents SDK Dockerfile, and a CI job
  that probes the guarantees from inside the container.
- `SECURITY.md` (reporting channel and threat model), a tag-triggered release
  workflow with prebuilt Linux/macOS binaries, `SHA256SUMS` and
  build-provenance attestations, and installation docs.
- Transport-fault scenarios: `retry-500`, `retry-503`, `retry-repeated`,
  `disconnect-after-request`, `connection-reset`, `truncated-response` and
  `slow-response`. A changed retry is `RetryMutation`; a runtime that gives
  up is UNKNOWN. The Agents SDK baselines record that a truncated SSE stream
  is not retried in streaming mode.
- `http_request` fuzz target for the provider's HTTP/1.1 parser.
- OpenAI Responses API (`provider_protocol: "openai-responses"`): request
  parser, response and SSE stream builders, server-side history for
  `previous_response_id`, and image comparison by decoded-byte hash.
  Built-in adapters `fixture-agent-responses` and
  `openai-agents-python-responses`, a Responses baseline, a Responses CI
  matrix entry, and Responses coverage in the false-PASS properties and the
  `provider_request` fuzz target.
- MCP content model (`src/model/content.rs`): ordered blocks (text, image,
  audio, resource_link, resource), `structuredContent`, `_meta` and JSON-RPC
  errors, with per-protocol `Representable` rules. New scenarios
  `multi-text-blocks`, `structured-content`, `error-with-metadata` and
  `mcp-protocol-error`, plus opt-in `--probes` (`mixed-content`,
  `embedded-resource`, `resource-link`, `audio-content`) and the UNKNOWN
  reason `unrepresentable-content`.
- A compatibility baseline for the OpenAI Agents SDK
  (`adapters/openai-agents-python/expected.json`), checked in CI by
  `scripts/check_expected.py`.
- Property tests that rule out false PASS results (`tests/false_pass.rs`): an
  independent oracle checks every PASS from simulated runs, which apply random
  legal and illegal transformations at delivery, retry and replay.
- `cargo-fuzz` targets for provider requests, MCP stdin, text classification
  and JSON comparison, plus a weekly fuzz workflow.
- The crate is now a library plus a thin binary, so tests and fuzzers can use
  its modules.
- `underlying_class` on `RetryMutation` / `ReplayMutation` findings, and
  `underlying_classifications` per scenario, so the proven cause (for example
  `MissingResult` or `WrongToolCallAssociation`) is not lost.
- Threshold-size scenarios around 64 KiB and 1 MiB, plus 250 KB, 1 MB and
  5 MB payloads, and a parameterized `large-text:<bytes>` scenario.
- `LICENSE-MIT` and `LICENSE-APACHE`.

### Changed
- The fake provider now runs its own minimal HTTP/1.1 server instead of
  `tiny_http`, for byte-level control of faults (one dependency fewer).
- CI actions are pinned by commit SHA; the Agents SDK adapter dependencies
  are hash-locked and the real-runtime run is part of CI.
- Output creation rejects symlinks in every user-controlled path component,
  and unredacted runtime logs are excluded unless explicitly requested.
- Compatibility checks now verify explicit streaming mode, runtime identity,
  version, verdicts, failure classes, and `UNKNOWN` reasons.
- CI uses a pinned Rust 1.85 toolchain, bounded jobs, and a consolidated
  real-runtime matrix to reduce Actions usage.

### Initial foundation

- OpenAI Chat Completions provider, MCP stdio server, deterministic scenarios,
  conforming and faulty fixtures, and the OpenAI Agents SDK adapter.
