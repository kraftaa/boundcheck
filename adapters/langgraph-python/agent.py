"""boundarycheck shim for LangGraph with langchain-mcp-adapters.

Wires the framework to the endpoints boundarycheck provides; tool-result
handling is entirely LangChain/LangGraph's:

  * model: langchain_openai.ChatOpenAI -> POST {OPENAI_BASE_URL}/chat/completions
           (--responses: use_responses_api=True -> POST {OPENAI_BASE_URL}/responses)
  * tools: langchain_mcp_adapters MultiServerMCPClient (stdio) -> the boundarycheck MCP server
  * loop:  langgraph.prebuilt.create_react_agent

--stream enables streaming=True on the model.
"""

import asyncio
import importlib.metadata as md
import json
import os
import platform
import sys

from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_openai import ChatOpenAI
from langgraph.prebuilt import create_react_agent


def write_runtime_info():
    path = os.environ.get("BOUNDARYCHECK_RUNTIME_INFO")
    if path:
        with open(path, "w") as f:
            json.dump({"name": "langgraph", "version": md.version("langgraph"),
                       "langchain-openai": md.version("langchain-openai"),
                       "langchain-mcp-adapters": md.version("langchain-mcp-adapters"),
                       "langchain-core": md.version("langchain-core"), "openai": md.version("openai"),
                       "mcp": md.version("mcp"), "python": platform.python_version()}, f)


async def main():
    write_runtime_info()
    flags = sys.argv[1:]
    client = MultiServerMCPClient({"boundarycheck": {
        "transport": "stdio",
        "command": os.environ["BOUNDARYCHECK_MCP_COMMAND"],
        "args": json.loads(os.environ["BOUNDARYCHECK_MCP_ARGS"]),
    }})
    tools = await client.get_tools()
    llm = ChatOpenAI(
        model="boundarycheck-model",
        base_url=os.environ["OPENAI_BASE_URL"],
        api_key=os.environ["OPENAI_API_KEY"],
        use_responses_api="--responses" in flags,
        streaming="--stream" in flags,
    )
    agent = create_react_agent(llm, tools)
    result = await agent.ainvoke({"messages": [("user", os.environ["BOUNDARYCHECK_PROMPT"])]})
    print(result["messages"][-1].content)


if __name__ == "__main__":
    asyncio.run(main())
