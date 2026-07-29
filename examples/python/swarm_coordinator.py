"""
swarm_coordinator.py — Swarm 多 Agent 协调示例

演示 SwarmCoordinator + SwarmSupervisor 的完整工作流：
  - 注册 worker（WorkerHandle）
  - 等待成员就绪（await_members）
  - 任务分配（assign_next_task）
  - 任务报告（report_completion）
  - 健康监控 + 终态等待（SwarmSupervisor）
  - 汇总报告（SwarmSummaryReport）

本示例模拟一个 3-worker 协作场景，无需真实 LLM 连接。
"""

import asyncio

from fox_agent_sdk import (
    SwarmCoordinator,
    SwarmSupervisor,
    RetryPolicy,
    WorkerStatus,
)


async def demo_basic_coordination():
    """基本的 worker 注册与生命周期管理。"""
    print("=" * 60)
    print("1. SwarmCoordinator — worker 注册")
    print("=" * 60)

    coord = SwarmCoordinator()

    # 注册三个 worker
    w1 = await coord.spawn("worker-1", "Analyze the project requirements")
    w2 = await coord.spawn("worker-2", "Design the architecture")
    w3 = await coord.spawn("worker-3", "Implement the core module")

    print(f"  Spawned: {w1.worker_id} (status={w1.status})")
    print(f"  Spawned: {w2.worker_id} (status={w2.status})")
    print(f"  Spawned: {w3.worker_id} (status={w3.status})")

    # 列出所有 worker
    workers = await coord.list_workers()
    print(f"\n  Total workers: {len(workers)}")
    for w in workers:
        print(f"    {w.worker_id:12s}  status={w.status:9s}  prompt={w.prompt[:40]}...")

    return coord, [w1, w2, w3]


async def demo_task_reporting(coord):
    """任务报告与汇总。"""
    print("\n" + "=" * 60)
    print("2. SwarmCoordinator — 任务报告")
    print("=" * 60)

    # Worker 完成任务后提交报告
    r1 = await coord.report_completion("worker-1", "task-1", "Requirements analyzed: 3 features identified")
    print(f"  Report: {r1.worker_id} → {r1.status} — {r1.summary[:50]}...")

    r2 = await coord.report_completion("worker-2", "task-2", "Architecture designed: microservices pattern")
    print(f"  Report: {r2.worker_id} → {r2.status} — {r2.summary[:50]}...")

    r3 = await coord.report_completion("worker-3", "task-3", "Core module: 500 lines of Rust written")
    print(f"  Report: {r3.worker_id} → {r3.status} — {r3.summary[:50]}...")

    # 获取所有报告
    reports = await coord.reports()
    print(f"\n  All reports: {len(reports)}")
    for r in reports:
        print(f"    {r.worker_id} [{r.status}] — {r.summary[:60]}")


async def demo_supervisor(coord):
    """Supervisor 监控 + 汇总报告。"""
    print("\n" + "=" * 60)
    print("3. SwarmSupervisor — 健康监控")
    print("=" * 60)

    # 自定义重试策略
    policy = RetryPolicy(
        max_retries=3,
        retry_delay_secs=2,
        reassign_on_exhausted=True,
        task_timeout_secs=300,
        health_check_interval_secs=5,
    )
    print(f"  Policy: max_retries={policy.max_retries}, timeout={policy.task_timeout_secs}s")

    supervisor = SwarmSupervisor(coord, policy)

    # 等待所有 worker 到达终态（这里手动完成了报告，所以 health loop 会很快返回）
    summary = await supervisor.await_completion()
    print(f"\n  Summary Report:")
    print(f"    Total workers:   {summary.total_workers}")
    print(f"    Completed:       {summary.completed}")
    print(f"    Failed:          {summary.failed}")
    print(f"    Timed out:       {summary.timed_out}")
    print(f"    Tasks reassigned:{summary.tasks_reassigned}")
    print(f"    All terminal:    {summary.all_terminal()}")

    # 可读格式
    print(f"\n  {summary.format()}")

    for r in summary.worker_reports:
        print(f"    - {r.worker_id}: [{r.status}] {r.summary[:50]}")


async def demo_with_defaults(coord):
    """快速创建默认策略的 supervisor。"""
    print("\n" + "=" * 60)
    print("4. SwarmSupervisor.with_defaults()")
    print("=" * 60)

    # 快捷构造：使用默认 RetryPolicy
    supervisor = SwarmSupervisor.with_defaults(coord)

    report = await supervisor.generate_summary()
    print(f"  Quick summary: completed={report.completed}, failed={report.failed}")
    print(f"  All terminal: {report.all_terminal()}")


# ─── main ───

async def main():
    # 1. 创建 coordinator 和 worker
    coord, workers = await demo_basic_coordination()

    # 2. 任务报告
    await demo_task_reporting(coord)

    # 3. Supervisor 监控
    await demo_supervisor(coord)

    # 4. 快捷默认策略
    await demo_with_defaults(coord)

    print("\n" + "=" * 60)
    print("swarm_coordinator demo done!")
    print("=" * 60)


if __name__ == "__main__":
    asyncio.run(main())
