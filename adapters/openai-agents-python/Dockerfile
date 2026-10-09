# Image for running the OpenAI Agents SDK adapter under --isolation docker:
#   docker build -t boundarycheck-openai-agents -f adapters/openai-agents-python/Dockerfile .
#   boundarycheck run --adapter openai-agents-python --isolation docker \
#     --image boundarycheck-openai-agents --isolation-binary <linux boundarycheck> \
#     -- python3 adapters/openai-agents-python/agent.py
# Dependencies come only from the hash-locked requirements.
FROM python:3.13-slim-trixie@sha256:70729b46c69b4f1e97c4822c1af3df53a1476cf5ddc6c087c0c10bc3a5678c2f
COPY adapters/openai-agents-python/requirements.lock /tmp/requirements.lock
RUN python -m pip install --no-cache-dir --require-hashes --no-deps -r /tmp/requirements.lock \
 && python -m pip check \
 && rm /tmp/requirements.lock
