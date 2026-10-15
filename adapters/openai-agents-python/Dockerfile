# Image for running the OpenAI Agents SDK adapter under --isolation docker:
#   docker build -t boundarycheck-openai-agents -f adapters/openai-agents-python/Dockerfile .
#   boundarycheck run --adapter openai-agents-python --isolation docker \
#     --image boundarycheck-openai-agents --isolation-binary <linux boundarycheck> \
#     -- python3 adapters/openai-agents-python/agent.py
# Dependencies come only from the hash-locked requirements.
FROM python:3.14-slim-trixie@sha256:a2b82f3c48559aa0a8446d9af49826b6e2b2016f4cd2afabfe6013ec53729170
COPY adapters/openai-agents-python/requirements.lock /tmp/requirements.lock
RUN python -m pip install --no-cache-dir --require-hashes --no-deps -r /tmp/requirements.lock \
 && python -m pip check \
 && rm /tmp/requirements.lock
