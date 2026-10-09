# boundarycheck

`boundarycheck` is a deterministic CLI test harness for agent runtimes. It answers:

> A tool produced X. What did the runtime actually send to the model provider?

It records both sides of the runtime independently and compares them without calling a real model:

```text
Fake MCP tool server  ->  runtime under test  ->  fake model provider
       records X                                records what arrived
```

- `PASS`: the result arrived intact.
- `FAIL`: boundarycheck proves a mutation, loss, duplication, truncation, or incorrect call association.
- `UNKNOWN`: the evidence is insufficient to decide safely.

## Install

From source (Rust 1.85 or later):

```bash
cargo install --git https://github.com/kraftaa/boundcheck --locked boundarycheck
```

Prebuilt Linux and macOS binaries, checksums, and provenance attestations are available on the [Releases page](https://github.com/kraftaa/boundcheck/releases).

## Quick start

```bash
cargo build --release
BC=./target/release/boundarycheck

# Conforming fixture: all supported scenarios pass
$BC run --adapter fixture-agent -- python3 fixtures/conforming-agent/agent.py

# Deliberately faulty fixture
$BC run --adapter fixture-agent -- \
  python3 fixtures/faulty-agent/agent.py --fault head-tail,swap
```

Save a machine-readable report with `--report boundarycheck-report.json`. Run `scripts/demo.sh` for the pinned real-runtime demonstration.

No provider credentials or real model calls are needed. Use boundarycheck with controlled test agents; use [Docker isolation](docs/usage.md#docker-isolation) for untrusted runtimes.

## What it checks

- exact, large, structured, and multi-block tool results;
- concurrent calls and call-ID association;
- retries, disconnects, resets, truncated responses, and slow responses;
- sequential, replayed, and persisted history;
- difficult Unicode, images, audio, and resources where the protocol permits them.

Both OpenAI Chat Completions and the OpenAI Responses API are supported. Results are based on raw boundary evidence, not runtime tracing or model judgment.

## Documentation

- [Usage, installation, CLI, scenarios, reports, and isolation](docs/usage.md)
- [Writing runtime adapters](docs/adapters.md)
- [JSON schemas for adapters and reports](schemas/)
- [Technical design, verdicts, and security model](docs/technical-design.md)
- [Development, tests, and fuzzing](docs/development.md)
- [Runtime compatibility matrix](docs/compatibility.md)
- [Recorded demonstration](docs/demo.md)
- [Security policy and threat model](SECURITY.md)

## Scope

boundarycheck supports function calling through OpenAI Chat Completions and Responses, with MCP over stdio. Some MCP content cannot be represented by every provider protocol; those opt-in probes return `UNKNOWN` rather than claiming a false failure or pass.

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or the [MIT License](LICENSE-MIT), at your option.
