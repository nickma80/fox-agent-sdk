use fox_agent_core::{
    AgentError, AgentEventTx, FoxAgentSdkConfig, Model, PlanStatus, SkillRegistry, TurnOutcome,
};
use fox_agent_swarm::{
    AgentReport, SwarmCoordinator, SwarmMessage, SwarmMessageKind, WorkerHandle, WorkerStatus,
};
use fox_agent_tools::{PlanItem, ToolExecutor, VersionedPlan, save_plan};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agent::Agent;
use crate::harness::Harness;

pub type SwarmAgentTx = AgentEventTx;

pub struct SwarmRuntime {
    pub coordinator: Arc<SwarmCoordinator>,
    pub model: Arc<dyn Model>,
    base_harness: Harness,
}

impl SwarmRuntime {
    /// Create a new SwarmRuntime from a coordinator, model, and a seed harness.
    ///
    /// The seed harness provides the tool registry, skill registry, and other
    /// configuration that will be shared (via cloning) across all forked agents.
    /// The coordinator is wired to the harness' planning store synchronously
    /// during construction — once `new` returns, all plan mutations are
    /// guaranteed to be persisted.
    pub async fn new(
        coordinator: Arc<SwarmCoordinator>,
        model: Arc<dyn Model>,
        base_harness: Harness,
    ) -> Self {
        // Wire coordinator persist to the harness' planning store before
        // returning — no race condition.
        let session_id = base_harness.session_id().to_string();
        let store = base_harness.planning_store.clone();
        coordinator.set_planning_store(store, session_id).await;
        Self {
            coordinator,
            model,
            base_harness,
        }
    }

    pub fn harness(&self) -> &Harness {
        &self.base_harness
    }

    /// Expose the tool executor for advanced use (e.g. registering more tools).
    pub fn tool_executor(&self) -> &ToolExecutor {
        self.base_harness.tool_executor()
    }

    pub fn skill_registry(&self) -> &Arc<RwLock<SkillRegistry>> {
        &self.base_harness.skill_registry
    }

    pub fn cfg(&self) -> &FoxAgentSdkConfig {
        &self.base_harness.cfg
    }

    /// Fork a new agent that shares the same tools, skills, and config but
    /// has an INDEPENDENT session state (so worker conversation does not
    /// pollute the parent session).
    pub async fn fork_agent(&self) -> Agent {
        let forked_model = self.model.fork();
        let forked_harness = self.base_harness.fork_session_state().await;
        Agent::new(
            forked_model,
            forked_harness,
            Arc::new(tokio::sync::RwLock::new(None)),
        )
    }

    pub async fn spawn_worker(
        &self,
        worker_id: impl Into<String>,
        prompt: impl Into<String>,
    ) -> WorkerHandle {
        self.coordinator.spawn(worker_id, prompt).await
    }

    pub async fn assign_next_runnable_task(&self, worker_id: &str) -> Option<PlanItem> {
        self.coordinator.assign_next_runnable_task(worker_id).await
    }

    pub async fn upsert_plan(&self, items: Vec<PlanItem>) -> VersionedPlan {
        self.coordinator.upsert_plan(items).await
    }

    pub async fn list_workers(&self) -> Vec<WorkerHandle> {
        self.coordinator.list_workers().await
    }

    pub async fn reports(&self) -> Vec<AgentReport> {
        self.coordinator.reports().await
    }
}

pub struct WorkerAgent {
    pub agent: Agent,
    pub coordinator: Arc<SwarmCoordinator>,
    pub worker_id: String,
}

impl WorkerAgent {
    pub fn new(agent: Agent, coordinator: Arc<SwarmCoordinator>, worker_id: String) -> Self {
        Self {
            agent,
            coordinator,
            worker_id,
        }
    }

    pub async fn drain_swarm_messages(&self) -> Vec<SwarmMessage> {
        self.coordinator.drain_inbox(&self.worker_id).await
    }

    pub async fn inject_inbox_into_session(&self) {
        let msgs = self.drain_swarm_messages().await;
        for msg in msgs {
            let from = &msg.from_worker_id;
            let prefix = match msg.kind {
                SwarmMessageKind::Broadcast => format!("[swarm broadcast from {from}]"),
                SwarmMessageKind::Direct => format!("[swarm dm from {from}]"),
            };
            let interrupt = format!("{prefix}\n\n{}", msg.content);
            self.agent
                .harness()
                .interrupt_manager
                .write()
                .await
                .queue_soft_interrupt(interrupt, false);
        }
    }

    pub async fn broadcast(&self, content: impl Into<String>) -> Vec<SwarmMessage> {
        self.coordinator.broadcast(&self.worker_id, content).await
    }

    pub async fn dm(
        &self,
        to_worker_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Option<SwarmMessage> {
        self.coordinator
            .dm(&self.worker_id, &to_worker_id.into(), content)
            .await
    }

    pub async fn run_once_streaming(
        &mut self,
        user_message: &str,
        event_tx: &SwarmAgentTx,
    ) -> Result<TurnOutcome, AgentError> {
        self.inject_inbox_into_session().await;
        self.agent.run_once_streaming(user_message, event_tx).await
    }

    fn sync_plan_to_session(&self) {
        let session_id = self.agent.harness().session_id();
        let items: Vec<PlanItem> = {
            let Ok(guard) = self.coordinator.shared_plan.try_read() else {
                return;
            };
            guard
                .items
                .iter()
                .map(|i| PlanItem {
                    id: i.id.clone(),
                    content: i.content.clone(),
                    status: PlanStatus::Pending,
                    priority: i.priority,
                    assigned_to: None,
                    blocked_by: i.blocked_by.clone(),
                })
                .collect()
        };
        if !items.is_empty() {
            save_plan(session_id, items, false);
        }
    }

    pub async fn try_assign_and_run(
        &mut self,
        event_tx: &SwarmAgentTx,
    ) -> Result<TurnOutcome, AgentError> {
        self.sync_plan_to_session();
        let plan_item = self
            .coordinator
            .assign_next_runnable_task(&self.worker_id)
            .await;
        let prompt = match plan_item {
            Some(item) => format!(
                "You are a swarm worker (id: {}).\n\n{}\n\nComplete this task autonomously.",
                self.worker_id, item.content
            ),
            None => format!(
                "You are a swarm worker (id: {}).\n\nNo runnable tasks available. Wait or broadcast status.",
                self.worker_id
            ),
        };
        self.run_once_streaming(&prompt, event_tx).await
    }

    pub async fn report_completion(
        &self,
        task_id: &str,
        summary: impl Into<String>,
    ) -> Option<AgentReport> {
        self.coordinator
            .report_completion(&self.worker_id, task_id, summary)
            .await
    }

    pub async fn worker_status(&self) -> Option<WorkerStatus> {
        let workers = self.coordinator.workers.read().await;
        workers.get(&self.worker_id).map(|h| h.status.clone())
    }

    pub fn harness(&self) -> &Harness {
        self.agent.harness()
    }
    pub fn model(&self) -> &Arc<dyn Model> {
        self.agent.model()
    }
}
