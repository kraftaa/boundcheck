# OpenAI Agents SDK adapter

- `requirements.txt`: the three pinned top-level packages (edit this to change versions).
- `requirements.lock`: the full, hash-locked dependency set used by CI. Regenerate it after editing `requirements.txt`:

  ```bash
  uv pip compile adapters/openai-agents-python/requirements.txt \
    --universal --python-version 3.13 --generate-hashes --no-header \
    -o adapters/openai-agents-python/requirements.lock
  ```

- Install exactly what CI tests:

  ```bash
  python3.13 -m venv .venv-agents
  .venv-agents/bin/python -m pip install --require-hashes --no-deps \
    -r adapters/openai-agents-python/requirements.lock
  ```
