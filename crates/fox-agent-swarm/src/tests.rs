#[cfg(test)]
mod swarm_tests {
    use super::*;
    use crate::*;
    use fox_agent_tools::{PlanItem, PlanPriority, PlanStatus};

    #[tokio::test]
    async fn assign_next_skips_blocked_items_until_dependencies_complete() {
        let coordinator = SwarmCoordinator::new();
        coordinator.spawn("w1", "worker").await;
        coordinator.upsert_plan(vec![
            PlanItem { id: "p1".into(), content: "first".into(), status: PlanStatus::Pending, priority: PlanPriority::High, assigned_to: None, blocked_by: vec![] },
            PlanItem { id: "p2".into(), content: "second".into(), status: PlanStatus::Pending, priority: PlanPriority::High, assigned_to: None, blocked_by: vec!["p1".into()] },
        ]).await;

        let first = coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert_eq!(first.id, "p1");
        coordinator.report_completion("w1", "p1", "done").await.unwrap();

        let second = coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert_eq!(second.id, "p2");
    }

    #[tokio::test]
    async fn broadcast_and_dm_deliver_messages() {
        let coordinator = SwarmCoordinator::new();
        coordinator.spawn("w1", "worker").await;
        coordinator.spawn("w2", "worker").await;

        let sent = coordinator.broadcast("w1", "hello").await;
        assert_eq!(sent.len(), 2);

        coordinator.dm("w1", "w2", "private").await.unwrap();

        let inbox = coordinator.drain_inbox("w2").await;
        assert!(inbox.iter().any(|m| m.kind == SwarmMessageKind::Broadcast && m.content == "hello"));
        assert!(inbox.iter().any(|m| m.kind == SwarmMessageKind::Direct && m.content == "private"));
    }

    #[tokio::test]
    async fn await_members_waits_until_expected_count() {
        let coordinator = SwarmCoordinator::new();
        coordinator.spawn("w1", "worker").await;

        let c = coordinator.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            c.spawn("w2", "worker").await;
        });

        let workers = coordinator.await_members(2, std::time::Duration::from_millis(500)).await.unwrap();
        assert!(workers.iter().any(|w| w.worker_id == "w1"));
        assert!(workers.iter().any(|w| w.worker_id == "w2"));
    }
}
