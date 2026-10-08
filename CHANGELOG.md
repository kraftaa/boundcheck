# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `underlying_class` on `RetryMutation` / `ReplayMutation` findings, and
  `underlying_classifications` per scenario, so the proven cause (for example
  `MissingResult` or `WrongToolCallAssociation`) is not lost.
- Threshold-size scenarios around 64 KiB and 1 MiB, plus 250 KB, 1 MB and
  5 MB payloads, and a parameterized `large-text:<bytes>` scenario.
- `LICENSE-MIT` and `LICENSE-APACHE`.

### Changed
- CI actions are pinned by commit SHA; the Agents SDK adapter dependencies
  are hash-locked and the real-runtime run is part of CI.

## [0.1.0]

First version: OpenAI Chat Completions provider, MCP stdio server,
deterministic scenarios, conforming and faulty fixtures, and the OpenAI
Agents SDK adapter.
