"""boundarycheck shim for the OpenAI Agents SDK (openai-agents, Python).

The shim only wires the SDK to the endpoints boundarycheck provides; all tool
result handling is done by the SDK itself:

  * model:  OpenAIChatCompletionsModel  -> POST {OPENAI_BASE_URL}/chat/completions
  * tools:  MCPServerStdio              -> the boundarycheck fake MCP server

Pass --stream to drive the agent with Runner.run_streamed (SSE responses).
"""

import asyncio
import importlib.metadata as md
import json
import os
import platform
import sys

from agents import Agent, OpenAIChatCompletionsModel, Runner, set_tracing_disabled
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
            model=OpenAIChatCompletionsModel(model="boundarycheck-model", openai_client=client),
        )
        prompt = os.environ["BOUNDARYCHECK_PROMPT"]
        if "--stream" in sys.argv[1:]:
            result = Runner.run_streamed(agent, prompt, max_turns=10)
            async for _event in result.stream_events():
                pass
        else:
            result = await Runner.run(agent, prompt, max_turns=10)
        print(result.final_output)


if __name__ == "__main__":
    asyncio.run(main())
