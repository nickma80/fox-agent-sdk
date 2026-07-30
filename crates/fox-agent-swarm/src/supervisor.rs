//! SwarmSupervisor: health checks, retry, reassignment, and timeout.
//!
//! Enhances [`SwarmCoordinator`] with production-grade lifecycle management:
//! - Periodic health checks on running workers
//! - Automatic retry of failed/timed-out tasks
//! - Task reassignment to a different worker on exhaustion
//! - Summary report generation

use crate::coordinator::SwarmCoordinator;
use crate::types::*;
use fox_agent_tools::PlanStatus;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant, sleep};

/// Tracks retry state per worker+task combination.
#[derive(Debug, Clone)]
pub struct RetryState {
    pub worker_id: String,
    pub task_id: String,
    pub attempts: u32,
    pub last_attempt_at: Instant,
}

/// A supervisor that wraps a [`SwarmCoordinator`] to provide
/// health checking, retry, reassignment, and reporting.
pub struct SwarmSupervisor {
    pub coordinator: Arc<SwarmCoordinator>,
    policy: RetryPolicy,
    pub retry_states: Arc<RwLock<HashMap<String, RetryState>>>,
    /// Counter for task reassignments
    reassignments: Arc<RwLock<u32>>,
}

impl SwarmSupervisor {
    /// Create a new supervisor wrapping the given coordinator.
    pub fn new(coordinator: Arc<SwarmCoordinator>, policy: RetryPolicy) -> Self {
        Self {
            coordinator,
            policy,
            retry_states: Arc::new(RwLock::new(HashMap::new())),
            reassignments: Arc::new(RwLock::new(0)),
        }
    }

    /// Create a supervisor with default retry policy.
    pub fn with_defaults(coordinator: Arc<SwarmCoordinator>) -> Self {
        Self::new(coordinator, RetryPolicy::default())
    }

    /// Get the retry policy.
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    // ── Health check loop ──

    /// Run a background health-check loop that monitors workers and handles
    /// timeouts. Returns when no workers remain in a non-terminal state.
    pub async fn run_health_loop(&self) {
        let interval = Duration::from_secs(self.policy.health_check_interval_secs);
        loop {
            sleep(interval).await;
            let workers = self.coordinator.list_workers().await;

            // Check for timed-out workers
            self.check_timeouts().await;

            // Check if all workers are terminal
            let all_done = workers.iter().all(|w| {
                matches!(
                    w.status,
                    WorkerStatus::Completed | WorkerStatus::Failed | WorkerStatus::TimedOut
                )
            });
            if all_done && !workers.is_empty() {
                break;
            }
        }
    }

    /// Check all running workers for timeout and mark them TimedOut if exceeded.
    async fn check_timeouts(&self) {
        if self.policy.task_timeout_secs == 0 {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let timeout = self.policy.task_timeout_secs;

        let retry_states = self.retry_states.read().await;
        let mut workers = self.coordinator.workers.write().await;

        for (worker_id, handle) in workers.iter_mut() {
            if handle.status != WorkerStatus::Running {
                continue;
            }

            // Determine the elapsed time since this worker began its current task.
            // Prefer retry state's last_attempt_at; fall back to handle.started_at_secs.
            let elapsed = if let Some(state) = retry_states.get(worker_id) {
                state.last_attempt_at.elapsed().as_secs()
            } else if let Some(started) = handle.started_at_secs {
                now.saturating_sub(started)
            } else {
                // No timestamp available — cannot determine timeout; skip.
                // This branch should not be reached in normal operation
                // because assign_next_runnable_task always sets started_at_secs.
                continue;
            };

            if elapsed > timeout {
                handle.status = WorkerStatus::TimedOut;
                let task_id = retry_states
                    .get(worker_id)
                    .map(|s| s.task_id.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                self.coordinator.reports.write().await.push(AgentReport {
                    worker_id: worker_id.clone(),
                    task_id: Some(task_id),
                    status: WorkerStatus::TimedOut,
                    summary: format!(
                        "Task timed out after {} seconds (limit: {})",
                        elapsed, timeout
                    ),
                });
            }
        }
    }

    // ── Retry handling ──

    /// Attempt to retry a failed or timed-out worker's task.
    ///
    /// Returns `true` if the task was retried (or reassigned).
    pub async fn handle_failure(&self, worker_id: &str, task_id: &str) -> bool {
        let mut states = self.retry_states.write().await;
        let key = format!("{worker_id}:{task_id}");

        let attempt = if let Some(state) = states.get(&key) {
            state.attempts + 1
        } else {
            1
        };

        if attempt > self.policy.max_retries {
            // Exhausted retries — reassign if policy allows
            if self.policy.reassign_on_exhausted {
                drop(states);
                return self.reassign_task(worker_id, task_id).await;
            }
            return false;
        }

        // Record retry attempt
        states.insert(
            key.clone(),
            RetryState {
                worker_id: worker_id.to_string(),
                task_id: task_id.to_string(),
                attempts: attempt,
                last_attempt_at: Instant::now(),
            },
        );
        drop(states);

        // Reset worker status to Ready so it can be picked up again
        let mut workers = self.coordinator.workers.write().await;
        if let Some(worker) = workers.get_mut(worker_id) {
            worker.status = WorkerStatus::Ready;
            worker.started_at_secs = None;
        }
        drop(workers);

        // Reset the plan item back to Pending
        let mut plan = self.coordinator.shared_plan.write().await;
        if let Some(item) = plan.items.iter_mut().find(|i| i.id == task_id) {
            item.status = PlanStatus::Pending;
            item.assigned_to = None;
        }

        // Apply retry delay
        if self.policy.retry_delay_secs > 0 {
            sleep(Duration::from_secs(self.policy.retry_delay_secs)).await;
        }

        true
    }

    /// Reassign a failed task to a different worker.
    async fn reassign_task(&self, failed_worker_id: &str, task_id: &str) -> bool {
        // Find a different worker that is available (Ready)
        let workers = self.coordinator.workers.read().await;
        let reassign_target = workers
            .iter()
            .find(|(id, w)| *id != failed_worker_id && w.status == WorkerStatus::Ready)
            .map(|(id, _)| id.clone());
        drop(workers);

        let target_id = match reassign_target {
            Some(id) => id,
            None => return false, // no available worker to reassign to
        };

        // Mark the plan item as Pending and unassign it
        let mut plan = self.coordinator.shared_plan.write().await;
        if let Some(item) = plan.items.iter_mut().find(|i| i.id == task_id) {
            item.status = PlanStatus::Pending;
            item.assigned_to = None;
        }
        drop(plan);

        // Mark the failed worker as Failed
        let mut workers = self.coordinator.workers.write().await;
        if let Some(worker) = workers.get_mut(failed_worker_id) {
            worker.status = WorkerStatus::Failed;
        }
        drop(workers);

        // Record reassignment
        *self.reassignments.write().await += 1;

        // Record a report for the failed worker
        self.coordinator.reports.write().await.push(AgentReport {
            worker_id: failed_worker_id.to_string(),
            task_id: Some(task_id.to_string()),
            status: WorkerStatus::Failed,
            summary: format!("Task reassigned to worker {target_id}"),
        });

        // Assign the task to the new worker
        self.coordinator.assign_next_runnable_task(&target_id).await;
        true
    }

    // ── Summary ──

    /// Generate a summary report from current coordinator state.
    pub async fn generate_summary(&self) -> SwarmSummaryReport {
        let reports = self.coordinator.reports().await;
        let mut summary = SwarmSummaryReport::from_reports(&reports);
        summary.tasks_reassigned = *self.reassignments.read().await;
        summary
    }

    /// Block until all workers reach a terminal state, then return the summary.
    pub async fn await_completion(&self) -> SwarmSummaryReport {
        self.run_health_loop().await;
        self.generate_summary().await
    }

    /// Trigger a health check on a specific worker.
    pub async fn check_worker_health(&self, worker_id: &str) -> Option<WorkerStatus> {
        let workers = self.coordinator.workers.read().await;
        workers.get(worker_id).map(|w| w.status.clone())
    }
}

// ── Tests ──

#[cfg(test)]
mod supervisor_tests {
    use super::*;
    use crate::coordinator::SwarmCoordinator;
    use fox_agent_tools::{PlanItem, PlanPriority, PlanStatus};

    #[tokio::test]
    async fn supervisor_retries_failed_task() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

        coordinator.spawn("w1", "worker").await;
        coordinator
            .upsert_plan(vec![PlanItem {
                id: "p1".into(),
                content: "task".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;

        let task = coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert_eq!(task.id, "p1");

        // Simulate failure
        let handled = supervisor.handle_failure("w1", "p1").await;
        assert!(handled, "failure should be handled with retry");

        // Worker should be Ready again for retry
        let workers = coordinator.list_workers().await;
        let w1 = workers.iter().find(|w| w.worker_id == "w1").unwrap();
        assert_eq!(w1.status, WorkerStatus::Ready);

        // Plan item should be back to Pending
        let plan = coordinator.shared_plan.read().await;
        let p1 = plan.items.iter().find(|i| i.id == "p1").unwrap();
        assert_eq!(p1.status, PlanStatus::Pending);
    }

    #[tokio::test]
    async fn supervisor_reassigns_after_max_retries() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let policy = RetryPolicy {
            max_retries: 1,
            retry_delay_secs: 0,
            reassign_on_exhausted: true,
            task_timeout_secs: 300,
            health_check_interval_secs: 5,
        };
        let supervisor = SwarmSupervisor::new(coordinator.clone(), policy);

        coordinator.spawn("w1", "worker a").await;
        coordinator.spawn("w2", "worker b").await;

        coordinator
            .upsert_plan(vec![PlanItem {
                id: "p1".into(),
                content: "task".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;

        coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert!(supervisor.handle_failure("w1", "p1").await);

        // After first retry: reset plan to Pending and assign to w1 again
        coordinator.assign_next_runnable_task("w1").await;

        // First verify the retry state was recorded
        {
            let states = supervisor.retry_states.read().await;
            assert!(states.contains_key("w1:p1"), "retry state should exist");
        }

        // Second failure — should reassign (attempts will be 2 > 1)
        let handled = supervisor.handle_failure("w1", "p1").await;
        assert!(handled, "reassign should succeed");

        // w1 should be Failed after reassignment
        let workers = coordinator.list_workers().await;
        let w1 = workers.iter().find(|w| w.worker_id == "w1").unwrap();
        assert_eq!(w1.status, WorkerStatus::Failed);

        let summary = supervisor.generate_summary().await;
        assert!(
            summary.tasks_reassigned > 0,
            "should have reassignments, got {}",
            summary.tasks_reassigned
        );
    }

    #[tokio::test]
    async fn summary_report_aggregates_correctly() {
        let coordinator = Arc::new(SwarmCoordinator::new());

        // Create two items in the plan
        coordinator
            .upsert_plan(vec![
                PlanItem {
                    id: "t1".into(),
                    content: "first task".into(),
                    status: PlanStatus::Pending,
                    priority: PlanPriority::High,
                    assigned_to: None,
                    blocked_by: vec![],
                },
                PlanItem {
                    id: "t2".into(),
                    content: "second task".into(),
                    status: PlanStatus::Pending,
                    priority: PlanPriority::High,
                    assigned_to: None,
                    blocked_by: vec![],
                },
            ])
            .await;

        // Spawn and assign tasks
        coordinator.spawn("w1", "worker").await;
        coordinator.spawn("w2", "worker").await;

        coordinator.assign_next_runnable_task("w1").await.unwrap();
        coordinator.report_completion("w1", "t1", "done a").await;

        coordinator.assign_next_runnable_task("w2").await.unwrap();
        // w2 fails
        coordinator.reports.write().await.push(AgentReport {
            worker_id: "w2".into(),
            task_id: Some("t2".into()),
            status: WorkerStatus::Failed,
            summary: "failed".into(),
        });

        let summary = SwarmSummaryReport::from_reports(&coordinator.reports().await);
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.timed_out, 0);
        assert!(summary.format().contains("2 workers total"));
    }

    #[tokio::test]
    async fn health_loop_detects_terminal_state() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let supervisor = SwarmSupervisor::with_defaults(coordinator.clone());

        coordinator.spawn("w1", "worker").await;
        coordinator
            .upsert_plan(vec![PlanItem {
                id: "p1".into(),
                content: "quick".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;

        coordinator.assign_next_runnable_task("w1").await.unwrap();
        coordinator
            .report_completion("w1", "p1", "done")
            .await
            .unwrap();

        let summary = supervisor.await_completion().await;
        assert_eq!(summary.completed, 1);
        assert!(summary.all_terminal());
    }

    /// First-run workers (no retry state) are timed out based on `started_at_secs`.
    #[tokio::test]
    async fn check_timeouts_handles_first_run_workers_without_retry_state() {
        let coordinator = Arc::new(SwarmCoordinator::new());
        let policy = RetryPolicy {
            task_timeout_secs: 1,
            max_retries: 0,
            retry_delay_secs: 0,
            reassign_on_exhausted: false,
            health_check_interval_secs: 0,
        };
        let supervisor = Arc::new(SwarmSupervisor::new(coordinator.clone(), policy));

        coordinator.spawn("w1", "worker").await;
        coordinator
            .upsert_plan(vec![PlanItem {
                id: "p1".into(),
                content: "task".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::High,
                assigned_to: None,
                blocked_by: vec![],
            }])
            .await;

        // Assign task — worker goes Running, started_at_secs is set
        coordinator.assign_next_runnable_task("w1").await.unwrap();
        assert!(
            supervisor.retry_states.read().await.is_empty(),
            "no retry state for first-run worker"
        );

        // Artificially age the start time to epoch → always exceeds timeout
        coordinator
            .workers
            .write()
            .await
            .get_mut("w1")
            .unwrap()
            .started_at_secs = Some(0);

        // Spawn health loop
        let sv = supervisor.clone();
        let handle = tokio::spawn(async move { sv.run_health_loop().await });

        // Wait for one tick + processing
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        handle.abort();

        let w1 = coordinator
            .list_workers()
            .await
            .into_iter()
            .find(|w| w.worker_id == "w1")
            .unwrap();
        assert_eq!(
            w1.status,
            WorkerStatus::TimedOut,
            "first-run worker with no retry state should be timed out"
        );
    }
}
