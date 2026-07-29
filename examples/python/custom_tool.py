"""
custom_tool.py — 自定义 Python Tool 和异步 Agent 使用演示

演示：
  - 用纯 Python 实现自定义 Tool（name/description/schema/execute）
  - 通过 AgentBuilder.with_tool() 注册
  - 异步 build() + 异步事件流

本示例可直接运行（无外部依赖）。
"""

import asyncio

from fox_agent_sdk import AgentBuilder, ProviderConfig, ToolOutput, ToolContext


class GreetTool:
    """A simple tool that greets a user by name."""

    def name(self) -> str:
        return "greet"

    def description(self) -> str:
        return "Greet a user by their name."

    def parameters_schema(self) -> dict:
        return {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Name to greet"},
            },
            "required": ["name"],
        }

    def execute(self, input: dict, ctx: ToolContext) -> ToolOutput:
        name = input.get("name", "World")
        return ToolOutput(text=f"Hello, {name}!")


async def main():
    # ─── 1. ToolOutput ───
    print("=" * 60)
    print("1. ToolOutput — 工具返回值")
    print("=" * 60)

    output = ToolOutput(text="Hello!")
    assert output.text == "Hello!"
    assert not output.is_error
    print(f"  Success: text='{output.text}', is_error={output.is_error}")

    error_output = ToolOutput.error("Something went wrong")
    assert error_output.is_error
    assert error_output.text == "Something went wrong"
    print(f"  Error:   text='{error_output.text}', is_error={error_output.is_error}")

    # ─── 2. Builder with custom tool ───
    print("\n" + "=" * 60)
    print("2. AgentBuilder.with_tool() — 注册自定义工具")
    print("=" * 60)

    cfg = ProviderConfig.deepseek("sk-test-key")
    builder = AgentBuilder()
    builder.provider_config(cfg)
    builder.model_id("deepseek-v4-flash")
    builder.working_dir(".")
    builder.with_tool(GreetTool())
    agent = await builder.build()

    print(f"  Session ID: {agent.session_id}")
    print(f"  Agent built with custom tool: GreetTool")

    # ─── 3. Async event stream ───
    print("\n" + "=" * 60)
    print("3. async for event in agent.run() — 事件流")
    print("=" * 60)

    try:
        async for event in agent.run("Hello!"):
            ev_type = event.get("type", "?")
            print(f"  [{ev_type}]")
    except Exception as e:
        err_str = str(e)
        if "Failed to" in err_str or "connection" in err_str.lower():
            print(f"  Expected: API 不可用（无真实 key）: {err_str[:120]}")
            print("  PASSED: Builder + tool 注册正确")
        else:
            print(f"  Error: {e}")

    print("\n" + "=" * 60)
    print("custom_tool demo done!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
