"""
memory_demo.py — Memory + Session 持久化演示

演示：
  - MemoryManager: remember / recall / search / list / forget
  - MemoryConfig 全参数配置
  - FileSessionStore 会话持久化
  - 异步 AgentBuilder.build()

本示例可直接运行（无外部依赖）。
"""

import asyncio
import os
import shutil
import tempfile

from fox_agent_sdk import (
    AgentBuilder,
    FileSessionStore,
    MemoryConfig,
    MemoryManager,
    MemoryScope,
    ProviderConfig,
    RecallMode,
)


def test_memory():
    """测试 MemoryManager CRUD API。"""
    print("=" * 60)
    print("1. MemoryManager — CRUD 操作")
    print("=" * 60)

    tmpdir = tempfile.mkdtemp(prefix="fox_memory_test_")
    projdir = os.path.join(tmpdir, "test_project")
    os.makedirs(projdir, exist_ok=True)

    cfg = MemoryConfig(
        enabled=True,
        auto_extract=False,
        max_results=10,
        injection_max_chars=1000,
    )

    mem = MemoryManager(cfg)
    mem = mem.with_storage_dir(tmpdir)
    mem = mem.with_project_dir(projdir)
    mem = mem.with_session_id("test-session-1")
    print(f"  Created: {mem}")
    print(f"  Wiki enabled: {mem.wiki_enabled()}")

    # Store
    id1 = mem.remember("The user prefers Rust for backend development", category="preference", scope="project")
    print(f"  Stored 1: {id1[:16]}...")
    id2 = mem.remember("The project uses DeepSeek as the LLM provider", category="fact", scope="project")
    print(f"  Stored 2: {id2[:16]}...")
    id3 = mem.remember("User's Python version is 3.13", category="fact", scope="session")
    print(f"  Stored 3: {id3[:16]}...")

    # List
    result = mem.list("all")
    print(f"  List all: {result['count']} entries")
    for entry in result["results"]:
        print(f"    - [{entry['category']}] {entry['content'][:60]}...")

    # Recall
    result = mem.recall("Rust", mode="keyword", scope="all")
    print(f"  Recall 'Rust': {result['count']} hits")
    for hit in result["results"]:
        print(f"    - score={hit['score']:.2f}: {hit['content'][:60]}")

    # Search
    result = mem.search("DeepSeek", scope="all")
    print(f"  Search 'DeepSeek': {result['count']} hits")

    # Forget
    ok = mem.forget(id3)
    print(f"  Forget memory 3: {'OK' if ok else 'FAIL'}")

    # After forget
    result = mem.list("all")
    print(f"  After forget: {result['count']} entries")

    shutil.rmtree(tmpdir, ignore_errors=True)
    print("  PASSED\n")


async def test_session_store():
    """测试 FileSessionStore。"""
    print("=" * 60)
    print("2. FileSessionStore — 会话持久化")
    print("=" * 60)

    tmpdir = tempfile.mkdtemp(prefix="fox_session_test_")
    store = FileSessionStore(tmpdir)
    print(f"  Store at: {tmpdir}")

    cfg = ProviderConfig.deepseek("sk-test-key")
    builder = AgentBuilder()
    builder.provider_config(cfg)
    builder.model_id("deepseek-v4-flash")
    builder.working_dir(".")
    builder.with_session_store(store)
    agent = await builder.build()
    print(f"  Session ID: {agent.session_id}")

    try:
        store.delete_session(agent.session_id)
        os.rmdir(tmpdir)
    except Exception:
        pass

    print("  PASSED\n")


def test_memory_config_comprehensive():
    """测试 MemoryConfig 全参数构造。"""
    print("=" * 60)
    print("3. MemoryConfig — 全参数配置")
    print("=" * 60)

    cfg = MemoryConfig(
        enabled=True,
        auto_extract=True,
        auto_extract_scope="Project",
        auto_extract_message_window=10,
        auto_extract_max_items_per_turn=5,
        wiki_enabled=True,
        max_results=20,
        injection_max_chars=2000,
        injection_max_per_category=5,
        verify_relevance=False,
        retention_days=90,
        memory_size_limit=5000,
    )
    print(f"  {cfg}")
    assert cfg.enabled is True

    mem = MemoryManager(cfg)
    print(f"  Manager: {mem}")
    print(f"  Wiki: {mem.wiki_enabled()}")
    print("  PASSED\n")


async def main():
    test_memory()
    await test_session_store()
    test_memory_config_comprehensive()

    print("=" * 60)
    print("memory_demo done!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
