# Usage guide

## Installation

Install from source with Rust 1.85 or later:

```bash
cargo install --git https://github.com/kraftaa/boundcheck --locked boundarycheck
```

Prebuilt Linux and macOS archives are attached to each [GitHub release](https://github.com/kraftaa/boundcheck/releases). Verify a downloaded archive with the published checksum and build attestation:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify boundarycheck-v0.1.0-aarch64-apple-darwin.tar.gz \
  --repo kraftaa/boundcheck
```

## Basic use

```text
boundarycheck run --adapter <name|path> [options] -- <agent-command> [args...]
boundarycheck list-scenarios
boundarycheck list-adapters
```

The command after `--` is launched directly, without a shell. Every scenario gets a fresh process and local fake endpoints on dynamically allocated `127.0.0.1` ports.

```bash
# All scenarios supported by the fixture adapter
boundarycheck run --adapter fixture-agent -- \
  python3 fixtures/conforming-agent/agent.py

# One scenario with a JSON report
boundarycheck run --adapter fixture-agent \
  --scenario large-text:65537 \
  --report boundarycheck-report.json -- \
  python3 fixtures/conforming-agent/agent.py
```

## Options

| Flag | Meaning |
|---|---|
| `--adapter <name|path>` | Required built-in adapter name or manifest path. |
| `--report <path>` | Write the JSON report. |
| `--artifacts <path>` | Save evidence for `FAIL` and `UNKNOWN` scenarios. |
| `--artifacts-all` | Also save passing-scenario evidence. |
| `--timeout <seconds>` | Per-scenario timeout, including startup. Default: 60. |
| `--scenario <name>` | Run one scenario; repeatable. |
| `--extended` | Add boundary sizes around 64 KiB and 1 MiB, plus 250 KiB and 5 MiB. |
| `--probes` | Add image, audio, resource, and resource-link probes. |
| `--keep-workdir` | Keep the temporary work directory, including unredacted runtime logs. |
| `--save-headers` | Save redacted request headers; requires `--artifacts`. |
| `--include-runtime-logs` | Include unredacted runtime stdout/stderr; may contain secrets. |
| `--isolation docker` | Run the harness and runtime in a locked-down container. |
| `--image <image>` | Container image required for Docker isolation. |
| `--isolation-binary <path>` | Linux boundarycheck binary to use inside the container. |
| `--container-engine <cli>` | Container CLI; default: `docker`. |

`large-text:<bytes>` accepts 1 KiB through 8 MiB, with optional `k` and `m` suffixes. `--extended`, `--probes`, and explicit `--scenario` selection are mutually constrained; the CLI reports invalid combinations.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Every selected scenario passed. |
| `1` | At least one scenario failed. |
| `2` | Harness, adapter, launch, or configuration error; also Ctrl-C. |
| `3` | No failure, but at least one scenario was unknown. |

## Scenario groups

### Content integrity

- `exact-text`
- `large-text-1k`, `large-text-50k`, `large-text-100k`, and `large-text:<bytes>`
- `structured-json`
- `unicode-boundaries`
- `multi-text-blocks`
- `structured-content`
- `mcp-error`, `error-with-metadata`, and `mcp-protocol-error`

### History and association

- `concurrent-two-tools`
- `sequential-history`
- `replay-history`
- `persistence-resume`

Replay and persistence scenarios require corresponding adapter capabilities.

### Transport faults

- `retry-429`, `retry-500`, `retry-503`, and `retry-repeated`
- `disconnect-after-request`
- `connection-reset`
- `truncated-response`
- `slow-response`

The provider records the complete incoming request before injecting a fault, so every retry can be compared with the original tool result.

### Optional probes

- `mixed-content`
- `embedded-resource`
- `resource-link`
- `audio-content`

A protocol that cannot represent a block may legitimately produce `UNKNOWN`. Text loss or mutation still produces `FAIL`.

## Reports and evidence

`--report` records run metadata, the adapter and runtime version, provider protocol, command with secrets redacted, per-scenario findings, hashes, request sequence, process state, timing, and totals.

`--artifacts DIR` saves raw evidence for failures and unknowns:

```text
DIR/BC_RUN_000001/
  run.json
  large-text-100k/
    tool-response-BC_CALL_000001.raw
    provider-request-002.raw
    provider-request-002.headers.json  # only with --save-headers
    runtime-stdout.log                 # only with --include-runtime-logs
    runtime-stderr.log                 # only with --include-runtime-logs
    comparison.json
```

Headers are opt-in and credentials are redacted. Runtime stdout/stderr are also
opt-in because arbitrary runtime logs can contain secrets; they are not redacted.
Raw evidence is capped at 8 MiB per file; stdout and stderr are capped at 1 MiB
each. Files use mode `0600`, directories use `0700`, and symlinks in every output
path component are refused. The report and adapter formats have published
[JSON schemas](../schemas/).

## Docker isolation

Isolation runs boundarycheck and the runtime together in a container with no network, a read-only root filesystem, no Linux capabilities, bounded processes, an in-memory `/tmp`, and only the current directory mounted read-only. Report and artifact directories remain writable.

```bash
# On macOS, first build a Linux boundarycheck binary
scripts/build-linux-binary.sh

docker build -t boundarycheck-openai-agents \
  -f adapters/openai-agents-python/Dockerfile .

boundarycheck run --adapter openai-agents-python \
  --isolation docker \
  --image boundarycheck-openai-agents \
  --isolation-binary target/linux-arm64/release/boundarycheck \
  --report out/report.json -- \
  python3 adapters/openai-agents-python/agent.py
```

The image must contain all runtime dependencies because downloads are disabled. Isolation protects the host from the runtime; it does not make the harness tamper-proof. See [SECURITY.md](../SECURITY.md).

## Real-runtime demonstration

`scripts/demo.sh` builds the project and runs the pinned OpenAI Agents SDK plus the conforming and deliberately faulty fixtures. See [demo.md](demo.md) for recorded output and [compatibility.md](compatibility.md) for the full runtime matrix.
