# Runtime adapters

An adapter is a JSON manifest that tells boundarycheck how to configure, start, drive, resume, and stop a runtime. The comparison engine remains independent of the runtime.

## Minimal shape

```json
{
  "name": "example-python-agent",
  "description": "free text",
  "provider_protocol": "openai-chat-completions",
  "capabilities": ["history-replay", "persistence-resume"],
  "environment": {
    "OPENAI_BASE_URL": "{{provider_base_url}}",
    "OPENAI_API_KEY": "boundarycheck-test-key",
    "BOUNDARYCHECK_MCP_COMMAND": "{{mcp_command}}",
    "BOUNDARYCHECK_MCP_ARGS": "{{mcp_args_json}}",
    "BOUNDARYCHECK_PROMPT": "{{scenario_prompt}}",
    "BOUNDARYCHECK_RUNTIME_INFO": "{{runtime_info_path}}",
    "BOUNDARYCHECK_STATE_PATH": "{{workdir}}/runtime-state.json"
  },
  "resume": {
    "environment": { "BOUNDARYCHECK_RESUME": "1" },
    "args": [],
    "stdin": null
  },
  "args": [],
  "files": [],
  "stdin": null,
  "readiness": { "type": "provider-request", "timeout_seconds": 30 },
  "completion": {
    "type": "provider-scenario-complete",
    "exit_grace_seconds": 5
  },
  "shutdown": { "grace_seconds": 3 },
  "runtime_version": { "type": "report-file" }
}
```

## Contract

| Requirement | Manifest field |
|---|---|
| Provider protocol | `provider_protocol`; use `openai-chat-completions` or `openai-responses`. |
| Provider base URL | Reference `{{provider_base_url}}` in `environment`, `args`, `files`, or `stdin`. |
| Dummy API key | Set a literal value in `environment`; real parent `OPENAI_*` values are not inherited. |
| MCP stdio registration | Use `{{mcp_command}}` and `{{mcp_args_json}}` in environment, arguments, or a generated file. |
| Initial user turn | Deliver `{{scenario_prompt}}` through environment, arguments, a file, or stdin. |
| Readiness | `provider-request` becomes ready when the first provider request arrives. |
| Completion | `provider-scenario-complete` or `process-exit`. |
| Shutdown | The runtime process group receives `SIGTERM`, then `SIGKILL` after the grace period. |
| Runtime version | `static`, `command`, `report-file`, or `unknown`. |
| Replay and resume | Declare capabilities; persistence also requires a `resume` block. |

## Templates

Available variables:

- `provider_base_url`
- `mcp_command`
- `mcp_args_json`
- `scenario_prompt`
- `scenario_id`
- `run_id`
- `workdir`
- `runtime_info_path`

`{{name_json}}` inserts a variable as a JSON string literal. Unknown variables are rejected before the runtime launches.

Generated file paths are rendered and then validated. They must be relative paths that remain inside the scenario work directory.

## Environment inheritance

The default, `inherit_environment: "minimal"`, passes only basic process settings such as `PATH`, `HOME`, user, shell, locale, temporary directory, time zone, and terminal variables. Add exact names with `environment_passthrough` when a runtime needs more.

`inherit_environment: "all"` still removes:

- `OPENAI_*` and `BOUNDARYCHECK_*` variables;
- names containing credential-like terms such as key, token, secret, auth, session, password, or credential.

Names in `environment_passthrough` are explicit exceptions. Adapter-defined variables are applied last. `NO_PROXY` and `no_proxy` include `127.0.0.1,localhost`.

## Lifecycle capabilities

- `history-replay`: the same process can resubmit its completed history.
- `persistence-resume`: the runtime can persist history, exit, and restore it in a new process.

If a capability is absent, its scenario is omitted by default and rejected when explicitly selected.

## Built-in adapters

- `fixture-agent`: the standard-library-only Python fixtures under `fixtures/`.
- `fixture-agent-responses`: the same fixtures using the Responses API; supports `--previous-response-id`.
- `openai-agents-python`: the pinned OpenAI Agents SDK over Chat Completions.
- `openai-agents-python-responses`: the same SDK using `OpenAIResponsesModel`.

Additional pinned adapters for Pydantic AI and LangGraph are exercised in the compatibility matrix. See [compatibility.md](compatibility.md).

List available adapters with:

```bash
boundarycheck list-adapters
```
