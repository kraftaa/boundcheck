"""boundarycheck shim for Pydantic AI.

Wires the framework to the endpoints boundarycheck provides; tool-result
handling is entirely Pydantic AI's:

  * model: OpenAIChatModel -> POST {OPENAI_BASE_URL}/chat/completions
           (--responses: OpenAIResponsesModel -> POST {OPENAI_BASE_URL}/responses)
  * tools: MCPToolset over a FastMCP StdioTransport -> the boundarycheck MCP server

--stream uses Agent.run_stream instead of Agent.run.
"""

import asyncio
import importlib.metadata as md
import json
import os
import platform
import sys

from fastmcp.client import Client
from fastmcp.client.transports import StdioTransport
from pydantic_ai import Agent
from pydantic_ai.mcp import MCPToolset
from pydantic_ai.models.openai import OpenAIChatModel, OpenAIResponsesModel
from pydantic_ai.providers.openai import OpenAIProvider


def version(dist):
    try:
        return md.version(dist)
    except md.PackageNotFoundError:
        return None


def write_runtime_info():
    path = os.environ.get("BOUNDARYCHECK_RUNTIME_INFO")
    if path:
        with open(path, "w") as f:
            json.dump({"name": "pydantic-ai", "version": version("pydantic-ai-slim"), "openai": version("openai"),
                       "mcp": version("mcp"), "fastmcp": version("fastmcp") or version("fastmcp-slim"),
                       "python": platform.python_version()}, f)


async def main():
    write_runtime_info()
    flags = sys.argv[1:]
    provider = OpenAIProvider(base_url=os.environ["OPENAI_BASE_URL"], api_key=os.environ["OPENAI_API_KEY"])
    model = (OpenAIResponsesModel if "--responses" in flags else OpenAIChatModel)("boundarycheck-model", provider=provider)
    transport = StdioTransport(
        command=os.environ["BOUNDARYCHECK_MCP_COMMAND"], args=json.loads(os.environ["BOUNDARYCHECK_MCP_ARGS"])
    )
    agent = Agent(model, toolsets=[MCPToolset(Client(transport))], instructions="Call tools exactly as instructed.")
    prompt = os.environ["BOUNDARYCHECK_PROMPT"]
    async with agent:
        if "--stream" in flags:
            async with agent.run_stream(prompt) as result:
                print(await result.get_output())
        else:
            print((await agent.run(prompt)).output)


if __name__ == "__main__":
    asyncio.run(main())
