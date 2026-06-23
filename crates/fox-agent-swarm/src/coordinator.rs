use fox_agent_tools::{PlanItem, PlanStatus, VersionedPlan};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, RwLock};

use crate::types::*;

/// In-process swarm coordinator for managing worker agents and shared plans.
#[derive(Clone)]
pub struct SwarmCoordinator {
    /// Shared versioned plan (all workers see the same plan)
    pub shared_plan: Arc<RwLock<VersionedPlan>>,
    /// Registry of all spawned workers
    pub workers: Arc<RwLock<HashMap<String, WorkerHandle>>>,
    /// Completion reports from workers
    pub reports: Arc<RwLock<Vec<AgentReport>>>,
    /// Message inboxes keyed by worker id
    pub inboxes: Arc<RwLock<HashMap<String, Vec<SwarmMessage>>>>,
    /// Notification signal for waiters (e.g. await_members)
    notify: Arc<Notify>,
}

impl Default for SwarmCoordinator {
    fn default() -> Self { Self::new() }
}

impl SwarmCoordinator {
    /// Create a new empty swarm coordinator.
    pub fn new() -> Self {
        Self {
            shared_plan: Arc::new(RwLock::new(VersionedPlan::default())),
            workers: Arc::new(RwLock::new(HashMap::new())),
            reports: Arc::new(RwLock::new(Vec::new())),
            inboxes: Arc::new(RwLock::new(HashMap::new())),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Register a new worker in the swarm.
    pub async fn spawn(&self, worker_id: impl Into<String>, prompt: impl Into<String>) -> WorkerHandle {
        let handle = WorkerHandle { worker_id: worker_id.into(), prompt: prompt.into(), status: WorkerStatus::Ready };
        self.workers.write().await.insert(handle.worker_id.clone(), handle.clone());
        self.inboxes.write().await.entry(handle.worker_id.clone()).or_default();
        self.notify.notify_waiters();
        handle
    }

    /// Replace the shared plan with new items (version bumped).
    pub async fn upsert_plan(&self, items: Vec<PlanItem>) -> VersionedPlan {
        let mut plan = self.shared_plan.write().await;
        plan.version += 1;
        plan.items = items;
        self.notify.notify_waiters();
        plan.clone()
    }

    /// Assign the next runnable (unblocked) task to a worker.
    pub async fn assign_next_runnable_task(&self, worker_id: &str) -> Option<PlanItem> {
        let mut plan = self.shared_plan.write().await;
        let completed_ids: Vec<_> = plan.items.iter()
            .filter(|i| i.status == PlanStatus::Completed).map(|i| i.id.clone()).collect();
        let next_idx = plan.items.iter().position(|item| {
            item.status == PlanStatus::Pending && item.blocked_by.iter().all(|b| completed_ids.iter().any(|d| d == b))
        })?;
        let item = &mut plan.items[next_idx];
        item.status = PlanStatus::InProgress;
        item.assigned_to = Some(worker_id.to_string());
        let assigned = item.clone();
        if let Some(worker) = self.workers.write().await.get_mut(worker_id) {
            worker.status = WorkerStatus::Running;
        }
        self.notify.notify_waiters();
        Some(assigned)
    }

    /// Mark a task as completed and record the worker's report.
    pub async fn report_completion(&self, worker_id: &str, task_id: &str, summary: impl Into<String>) -> Option<AgentReport> {
        let mut plan = self.shared_plan.write().await;
        let item = plan.items.iter_mut().find(|i| i.id == task_id)?;
        item.status = PlanStatus::Completed;
        if let Some(worker) = self.workers.write().await.get_mut(worker_id) {
            worker.status = WorkerStatus::Completed;
        }
        let report = AgentReport { worker_id: worker_id.to_string(), task_id: Some(task_id.to_string()), status: WorkerStatus::Completed, summary: summary.into() };
        self.reports.write().await.push(report.clone());
        self.notify.notify_waiters();
        Some(report)
    }

    /// Broadcast a message to all workers.
    pub async fn broadcast(&self, from_worker_id: &str, content: impl Into<String>) -> Vec<SwarmMessage> {
        let content = content.into();
        let worker_ids: Vec<_> = self.workers.read().await.keys().cloned().collect();
        let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut created = Vec::new();
        let mut inboxes = self.inboxes.write().await;
        for to in worker_ids {
            let msg = SwarmMessage {
                id: format!("m-{}-{}", from_worker_id, now_secs),
                kind: SwarmMessageKind::Broadcast,
                from_worker_id: from_worker_id.to_string(),
                to_worker_id: Some(to.clone()),
                content: content.clone(),
                at_secs: now_secs,
            };
            inboxes.entry(to).or_default().push(msg.clone());
            created.push(msg);
        }
        self.notify.notify_waiters();
        created
    }

    /// Send a direct message to a specific worker.
    pub async fn dm(&self, from_worker_id: &str, to_worker_id: &str, content: impl Into<String>) -> Option<SwarmMessage> {
        if !self.workers.read().await.contains_key(to_worker_id) { return None; }
        let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let msg = SwarmMessage {
            id: format!("m-{}-{}-{}", from_worker_id, to_worker_id, now_secs),
            kind: SwarmMessageKind::Direct,
            from_worker_id: from_worker_id.to_string(),
            to_worker_id: Some(to_worker_id.to_string()),
            content: content.into(),
            at_secs: now_secs,
        };
        self.inboxes.write().await.entry(to_worker_id.to_string()).or_default().push(msg.clone());
        self.notify.notify_waiters();
        Some(msg)
    }

    /// Drain and return all messages from a worker's inbox.
    pub async fn drain_inbox(&self, worker_id: &str) -> Vec<SwarmMessage> {
        let mut inboxes = self.inboxes.write().await;
        inboxes.remove(worker_id).unwrap_or_default()
    }

    /// Block until at least expected_count workers have been spawned, or timeout.
    pub async fn await_members(&self, expected_count: usize, timeout: Duration) -> Option<Vec<WorkerHandle>> {
        let wait = async {
            loop {
                let current = self.list_workers().await;
                if current.len() >= expected_count { return current; }
                self.notify.notified().await;
            }
        };
        tokio::time::timeout(timeout, wait).await.ok()
    }

    /// List all registered workers.
    pub async fn list_workers(&self) -> Vec<WorkerHandle> {
        self.workers.read().await.values().cloned().collect()
    }

    /// Get all completion reports.
    pub async fn reports(&self) -> Vec<AgentReport> {
        self.reports.read().await.clone()
    }
}
