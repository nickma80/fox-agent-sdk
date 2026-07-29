"""
mcp_demo.py — MCP (Model Context Protocol) 配置演示

演示：
  - McpServerConfig.stdio() / McpServerConfig.sse() 工厂方法
  - AgentBuilder.with_mcp_server() 注册
  - 异步 build()

本示例可直接运行（无外部依赖）。
"""

import asyncio

from fox_agent_sdk import AgentBuilder, McpServerConfig, McpTransportMode, ProviderConfig


async def main():
    # ─── 1. Transport Modes ───
    print("=" * 60)
    print("1. McpTransportMode")
    print("=" * 60)

    # Stdio
    t1 = McpTransportMode.stdio(
        command="node",
        args=["server.js"],
        cwd="/tmp",
        startup_grace_ms=10000,
    )
    print(f"  Stdio:  {t1}")

    # SSE
    t2 = McpTransportMode.sse(
        url="http://localhost:8080/sse",
        headers=[("Authorization", "Bearer token")],
        connect_timeout_secs=30,
    )
    print(f"  SSE:    {t2}")

    # ─── 2. McpServerConfig ───
    print("\n" + "=" * 60)
    print("2. McpServerConfig — 工厂方法")
    print("=" * 60)

    # Stdio config
    stdio_cfg = McpServerConfig.stdio(
        name="filesystem",
        command="npx",
        args=["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
        auto_approve=True,
        tools_only=["read_file", "list_directory"],
    )
    print(f"  Stdio:  {stdio_cfg}")

    # SSE config
    sse_cfg = McpServerConfig.sse(
        name="remote-tools",
        url="http://localhost:8080/sse",
        auto_approve=False,
        headers=[("Authorization", "Bearer my-token")],
    )
    print(f"  SSE:    {sse_cfg}")

    # ─── 3. Builder with MCP ───
    print("\n" + "=" * 60)
    print("3. AgentBuilder.with_mcp_server() — 注册 MCP 服务器")
    print("=" * 60)

    cfg = ProviderConfig.deepseek("sk-test-key")
    builder = AgentBuilder()
    builder.provider_config(cfg)
    builder.model_id("deepseek-v4-flash")
    builder.working_dir(".")

    mcp = McpServerConfig.stdio(
        name="my-server",
        command="node",
        args=["server.js"],
        auto_approve=True,
    )
    builder.with_mcp_server(mcp)
    agent = await builder.build()

    print(f"  Session ID: {agent.session_id}")
    print(f"  Agent with MCP server built successfully!")

    print("\n" + "=" * 60)
    print("mcp_demo done!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
