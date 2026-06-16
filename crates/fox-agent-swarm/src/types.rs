use serde::{Deserialize, Serialize};

/// Lifecycle status of a swarm worker agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Spawned but not yet assigned a task
    Ready,
    /// Currently executing a task
    Running,
    /// Blocked waiting for dependencies
    Blocked,
    /// Successfully completed its assigned task
    Completed,
    /// Task execution failed
    Failed,
}

/// A handle representing a registered worker in the swarm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerHandle {
    /// Unique worker identifier
    pub worker_id: String,
    /// Initial prompt or role description for the worker
    pub prompt: String,
    /// Current lifecycle status
    pub status: WorkerStatus,
}

/// A completion report from a worker agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentReport {
    /// Which worker produced this report
    pub worker_id: String,
    /// The task id that was completed (None if no task was assigned)
    pub task_id: Option<String>,
    /// Outcome status
    pub status: WorkerStatus,
    /// Human-readable summary of what was done
    pub summary: String,
}

/// Classification of a swarm message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmMessageKind {
    /// Sent to all workers
    Broadcast,
    /// Sent to a specific worker
    Direct,
}

/// A message exchanged between swarm members.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmMessage {
    /// Unique message id
    pub id: String,
    /// Broadcast or direct
    pub kind: SwarmMessageKind,
    /// Sender worker id
    pub from_worker_id: String,
    /// Target worker id (Some for direct, None for broadcasts)
    pub to_worker_id: Option<String>,
    /// Message content
    pub content: String,
    /// Unix timestamp when the message was sent
    pub at_secs: u64,
}
