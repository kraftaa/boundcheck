# Image for running the OpenAI Agents SDK adapter under --isolation docker:
#   docker build -t boundarycheck-openai-agents -f adapters/openai-agents-python/Dockerfile .
#   boundarycheck run --adapter openai-agents-python --isolation docker \
#     --image boundarycheck-openai-agents --isolation-binary <linux boundarycheck> \
#     -- python3 adapters/openai-agents-python/agent.py
# Dependencies come only from the hash-locked requirements.
FROM python:3.15-slim-trixie@sha256:8c13a3f37a6a5b71498228e902c4b9f51c1f7900d49fabf71f9a650c6120e0d0
COPY adapters/openai-agents-python/requirements.lock /tmp/requirements.lock
RUN python -m pip install --no-cache-dir --require-hashes --no-deps -r /tmp/requirements.lock \
 && python -m pip check \
 && rm /tmp/requirements.lock
