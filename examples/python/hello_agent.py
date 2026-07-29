"""
hello_agent.py — 异步 Agent 基本用法演示

展示新 Phase 3 异步 API：
  - await builder.build()  异步构建
  - async for event in agent.run(...)  异步事件流
  - agent.resume(allow=True)  权限恢复
  - await agent.snapshot()  会话快照

运行依赖：pip install fox-agent-sdk (加上有效的 API key)
无 API key 时仅演示 API 形态，不会真正调用 LLM。
"""

import asyncio

from fox_agent_sdk import AgentBuilder, ProviderConfig, EventType


# ─── 模拟事件流（不需要真实 API key 也能运行） ───

async def demo_event_stream():
    """演示 async for event in agent.run() 的事件流模式。"""
    print("=" * 60)
    print("1. EventStream — 异步事件流")
    print("=" * 60)

    # 如果已通过 maturin develop 安装，可以取消注释实际调用：
    #
    # agent = await (
    #     AgentBuilder()
    #     .provider_config(ProviderConfig.deepseek("sk-your-key"))
    #     .model_id("deepseek-v4-flash")
    #     .working_dir("./demo_workspace")
    #     .with_default_tools()
    #     .build()
    # )
    # async for event in agent.run("Create a hello.py"):
    #     match event.get("type"):
    #         case "text_delta":   print(event["text"], end="")
    #         case "tool_start":   print(f"\n[Tool: {event['name']}]")
    #         case "tool_end":     ...
    #         case "turn_start":   print(f"\n--- Turn {event['turn_id']} ---")
    #         case "turn_end":     print(f"\n[Done: {event.get('outcome')}]")
    #         case "error":        print(f"\n[Error: {event['error']}]")
    #         case "usage":        print(f"\n[Tokens: {event['input']}→{event['output']}]")

    print("  async for event in agent.run('Create a hello.py'):")
    print("      match event['type']:")
    print("          case 'text_delta':  print(event['text'])")
    print("          case 'tool_start':  print(f'[Using {event[\"name\"]}]')")
    print("          case 'tool_end':    ...")
    print("          case 'turn_end':    print('Done')")
    print("          case 'error':       print(f'Error: {event[\"error\"]}')")
    print()


# ─── 权限恢复 ───

async def demo_permission_resume():
    """演示 agent.resume() 权限恢复流程。"""
    print("=" * 60)
    print("2. Permission Resume — 权限恢复")
    print("=" * 60)

    print("""
    # 当 agent 需要用户确认（如文件写入）时，会发送：
    # {"type": "turn_end", "outcome": "requires_user_decision", "request": {...}}
    #
    # Python 端检测到此事件后停止迭代，调用 resume():
    #
    #   async for event in agent.run("Delete all files"):
    #       if event.get("outcome") == "requires_user_decision":
    #           break
    #   # 用户检查后决定：
    #   async for event in agent.resume(allow=True):  # 或 allow=False
    #       ...
    """)


# ─── Session Snapshot ───

async def demo_snapshot():
    """演示 agent.snapshot() 会话持久化。"""
    print("=" * 60)
    print("3. Session Snapshot — 会话快照")
    print("=" * 60)

    print("""
    # 运行过程中保存快照：
    #   snapshot = await agent.snapshot()
    #   print(snapshot.session_id, snapshot.turn_count)
    #
    # 配合 FileSessionStore 实现跨进程恢复：
    #   store = FileSessionStore("./sessions")
    #   builder.with_session_store(store)
    """)


# ─── Event Type 常量 ───

def demo_event_types():
    """展示 EventType 常量，方便 match/case。"""
    print("=" * 60)
    print("4. EventType 常量速查")
    print("=" * 60)

    all_types = [
        ("TEXT_DELTA",       EventType.TEXT_DELTA),
        ("THINKING_DELTA",   EventType.THINKING_DELTA),
        ("TOOL_START",       EventType.TOOL_START),
        ("TOOL_END",         EventType.TOOL_END),
        ("TOOL_PROGRESS",    EventType.TOOL_PROGRESS),
        ("USAGE",            EventType.USAGE),
        ("ERROR",            EventType.ERROR),
        ("PERMISSION_REQUEST", EventType.PERMISSION_REQUEST),
        ("TURN_START",       EventType.TURN_START),
        ("TURN_END",         EventType.TURN_END),
        ("ARTIFACT_STORED",  EventType.ARTIFACT_STORED),
        ("ARTIFACT_READ",    EventType.ARTIFACT_READ),
        ("MCP_CONNECTED",    EventType.MCP_CONNECTED),
        ("MCP_DISCONNECTED", EventType.MCP_DISCONNECTED),
        ("PLAN_PROGRESS",    EventType.PLAN_PROGRESS),
    ]

    for name, value in all_types:
        print(f"  EventType.{name:<20s} = {value!r}")
    print(f"\n  {len(all_types)} event types total")


# ─── main ───

async def main():
    await demo_event_stream()
    await demo_permission_resume()
    await demo_snapshot()
    demo_event_types()

    print("\n" + "=" * 60)
    print("hello_agent demo done — 配置 API key 后即可实际运行")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
