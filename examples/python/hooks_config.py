"""
hooks_config.py — Hooks、Plugins 与 Skills 配置演示

展示 Phase 3 新增的扩展能力：
  - HookEvent 生命周期事件常量
  - HooksConfig 钩子配置
  - PluginsConfig 插件市场与预安装
  - PluginManifest 元数据
  - Skill / SkillRegistry 技能查询

本示例不依赖外部 API，纯配置/DTO 展示。
"""

from fox_agent_sdk import (
    HookEvent,
    HooksConfig,
    PluginsConfig,
    PluginManifest,
)


# ─── 1. HookEvent — 生命周期事件 ───

def demo_hook_events():
    """展示所有 11 个 HookEvent 常量。"""
    print("=" * 60)
    print("1. HookEvent — 生命周期事件常量")
    print("=" * 60)

    events = [
        ("SESSION_START",       HookEvent.SESSION_START,       "会话开始时"),
        ("USER_PROMPT_SUBMIT",  HookEvent.USER_PROMPT_SUBMIT,  "用户提交提示后"),
        ("PRE_TOOL_USE",        HookEvent.PRE_TOOL_USE,        "工具执行前"),
        ("POST_TOOL_USE",       HookEvent.POST_TOOL_USE,       "工具执行后"),
        ("NOTIFICATION",        HookEvent.NOTIFICATION,        "单向通知"),
        ("STOP",                HookEvent.STOP,                "Agent 停止"),
        ("SUBAGENT_STOP",       HookEvent.SUBAGENT_STOP,       "子 Agent 完成"),
        ("PRE_COMPACT",         HookEvent.PRE_COMPACT,         "上下文压缩前"),
        ("PERMISSION_PROMPT",   HookEvent.PERMISSION_PROMPT,   "权限提示"),
        ("PRE_FILE_WRITE",      HookEvent.PRE_FILE_WRITE,      "写入文件前"),
        ("POST_FILE_WRITE",     HookEvent.POST_FILE_WRITE,     "写入文件后"),
    ]

    for name, value, desc in events:
        print(f"  HookEvent.{name:<21s} = {value!r:<22s} // {desc}")


# ─── 2. HooksConfig — 钩子配置 ───

def demo_hooks_config():
    """演示 HooksConfig 构造 + 自定义目录。"""
    print("\n" + "=" * 60)
    print("2. HooksConfig — 钩子系统配置")
    print("=" * 60)

    # 默认配置
    default_cfg = HooksConfig()
    print(f"  Default: enabled=True, timeout=30s, max_concurrent=5")

    # 自定义超时和并发
    custom_cfg = HooksConfig(
        enabled=True,
        timeout_secs=60,
        max_concurrent=10,
        load_global=False,
    )
    print(f"  Custom:  enabled=True, timeout=60s, max_concurrent=10, load_global=False")

    # 添加额外目录扫描 hooks
    custom_cfg.add_directory("./project/.claude/hooks")
    custom_cfg.add_directory("/etc/fox-agent/hooks")
    print(f"  Added dirs: ./project/.claude/hooks, /etc/fox-agent/hooks")
    print(f"  Repr: {custom_cfg}")


# ─── 3. PluginsConfig — 插件配置 ───

def demo_plugins_config():
    """演示 PluginsConfig 构造。"""
    print("\n" + "=" * 60)
    print("3. PluginsConfig — 插件系统配置")
    print("=" * 60)

    cfg = PluginsConfig(enabled=True, auto_update_hours=24)

    # 预安装插件
    cfg.add_preinstall("code-reviewer")
    cfg.add_preinstall("security-scanner")
    print(f"  Preinstall: code-reviewer, security-scanner")

    # 注册 marketplace
    cfg.add_marketplace(
        name="official",
        url="https://plugins.fox-agent.dev/index.json",
        source="Http",
    )
    cfg.add_marketplace(
        name="community",
        url="https://github.com/fox-agent/community-plugins",
        source="GitHub",
        owner="fox-agent",
        repo="community-plugins",
    )
    print(f"  Marketplaces: official (Http), community (GitHub)")
    print(f"  Repr: {cfg}")


# ─── 4. PluginManifest — 元数据视图 ───

def demo_plugin_manifest():
    """展示 PluginManifest 的字段结构。"""
    print("\n" + "=" * 60)
    print("4. PluginManifest — 插件元数据字段")
    print("=" * 60)

    print("""
  Plugin 的 plugin.json 示例：
  {
      "name": "code-reviewer",
      "version": "1.2.0",
      "description": "AI-powered code review with best practices",
      "author": "Fox Agent Team",
      "repository": "https://github.com/fox-agent/code-reviewer",
      "min_sdk_version": "0.1.0",
      "entry": {
          "skills": ["skills/"]
      },
      "dependencies": {}
  }

  Python 侧通过 PluginManifest 只读属性访问：
    manifest.name          → "code-reviewer"
    manifest.version       → "1.2.0"
    manifest.description   → "AI-powered code review..."
    manifest.author        → "Fox Agent Team"
    manifest.repository    → "https://github.com/..."
""")


# ─── 5. Skills 集成提示 ───

def demo_skills_integration():
    """提示 Skills 与 Agent 的集成方式。"""
    print("=" * 60)
    print("5. Skills + Agent 集成")
    print("=" * 60)

    print("""
  # Agent build 时自动加载 .claude/skills/ 下的 skill：
  #   agent = await AgentBuilder()
  #       .with_default_tools()
  #       .build()
  #
  # 运行时查询：
  #   registry = agent.skill_registry
  #   for skill in registry.list():
  #       print(f"{skill.name}: {skill.description}")
  #       print(f"  Allowed tools: {skill.allowed_tools}")
  #       if skill.model:
  #           print(f"  Model override: {skill.model}")
""")


# ─── main ───

def main():
    demo_hook_events()
    demo_hooks_config()
    demo_plugins_config()
    demo_plugin_manifest()
    demo_skills_integration()

    print("=" * 60)
    print("hooks_config demo done!")
    print("=" * 60)


if __name__ == "__main__":
    main()
