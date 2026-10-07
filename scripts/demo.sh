#!/bin/sh
# Reproducible terminal demonstration (see docs/demo.md for a recorded run).
#
#   1. a real runtime (OpenAI Agents SDK, pinned) — expected: all PASS
#   2. the faulty fixture with deliberately injected faults — expected: FAIL
#
# Prerequisites: cargo, python3, uv (only for step 1).
set -u
cd "$(dirname "$0")/.."
cargo build --release -q || exit 2
BC=./target/release/boundarycheck

if [ ! -x .venv-agents/bin/python ]; then
  uv venv -q .venv-agents --python 3.13 &&
    VIRTUAL_ENV=.venv-agents uv pip install -q -r adapters/openai-agents-python/requirements.txt || exit 2
fi

run() {
  echo "\$ boundarycheck $*"
  "$BC" "$@"
  echo "[exit status $?]"
  echo
}

run run --adapter openai-agents-python --report boundarycheck-report.json \
  -- .venv-agents/bin/python adapters/openai-agents-python/agent.py

run run --adapter fixture-agent --scenario exact-text --scenario large-text-100k --scenario concurrent-two-tools \
  -- python3 fixtures/faulty-agent/agent.py --fault head-tail,swap

run run --adapter fixture-agent --scenario retry-429 --scenario structured-json \
  -- python3 fixtures/faulty-agent/agent.py --fault retry-mutation,json-float
