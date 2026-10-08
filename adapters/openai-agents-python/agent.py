"""boundarycheck shim for the OpenAI Agents SDK (openai-agents, Python).

The shim only wires the SDK to the endpoints boundarycheck provides; all tool
result handling is done by the SDK itself:

  * model:  OpenAIChatCompletionsModel  -> POST {OPENAI_BASE_URL}/chat/completions
            (with --responses: OpenAIResponsesModel -> POST {OPENAI_BASE_URL}/responses)
  * tools:  MCPServerStdio              -> the boundarycheck fake MCP server

Pass --stream to drive the agent with Runner.run_streamed (SSE responses).
"""

import asyncio
import importlib.metadata as md
import json
import os
import platform
import sys

from agents import (
    Agent,
    OpenAIChatCompletionsModel,
    OpenAIResponsesModel,
    Runner,
    SQLiteSession,
    set_tracing_disabled,
)
from agents.mcp import MCPServerStdio
from openai import AsyncOpenAI


def write_runtime_info():
    path = os.environ.get("BOUNDARYCHECK_RUNTIME_INFO")
    if not path:
        return
    info = {
        "name": "openai-agents",
        "version": md.version("openai-agents"),
        "openai": md.version("openai"),
        "mcp": md.version("mcp"),
        "python": platform.python_version(),
    }
    with open(path, "w") as f:
        json.dump(info, f)


async def run_once(agent, prompt, session, streamed):
    if streamed:
        result = Runner.run_streamed(agent, prompt, max_turns=10, session=session)
        async for _event in result.stream_events():
            pass
        return result
    return await Runner.run(agent, prompt, max_turns=10, session=session)


async def main():
    write_runtime_info()
    set_tracing_disabled(True)
    client = AsyncOpenAI(base_url=os.environ["OPENAI_BASE_URL"], api_key=os.environ["OPENAI_API_KEY"])
    params = {
        "command": os.environ["BOUNDARYCHECK_MCP_COMMAND"],
        "args": json.loads(os.environ["BOUNDARYCHECK_MCP_ARGS"]),
    }
    async with MCPServerStdio(name="boundarycheck", params=params, client_session_timeout_seconds=30) as server:
        agent = Agent(
            name="boundarycheck-agent",
            instructions="Call tools exactly as instructed.",
            mcp_servers=[server],
            model=(OpenAIResponsesModel if "--responses" in sys.argv[1:] else OpenAIChatCompletionsModel)(
                model="boundarycheck-model", openai_client=client
            ),
        )
        prompt = os.environ["BOUNDARYCHECK_PROMPT"]
        scenario = os.environ["BOUNDARYCHECK_SCENARIO_ID"]
        lifecycle = scenario in {"replay-history", "persistence-resume"}
        session = (
            SQLiteSession(os.environ["BOUNDARYCHECK_SESSION_ID"], os.environ["BOUNDARYCHECK_SESSION_DB"])
            if lifecycle
            else None
        )
        resumed = os.environ.get("BOUNDARYCHECK_RESUME") == "1"
        if resumed:
            prompt = "Continue from the persisted boundarycheck history without calling the tool again."
        result = await run_once(agent, prompt, session, "--stream" in sys.argv[1:])
        if result.final_output == "BOUNDARYCHECK_REPLAY_REQUIRED":
            result = await run_once(
                agent,
                "Replay the existing history without calling the tool again.",
                session,
                "--stream" in sys.argv[1:],
            )
        print(result.final_output)


if __name__ == "__main__":
    asyncio.run(main())
