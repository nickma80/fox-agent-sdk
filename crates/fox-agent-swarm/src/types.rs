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
    /// Task execution timed out before completion
    TimedOut,
}

/// A handle representing a registered worker in the swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHandle {
    /// Unique worker identifier
    pub worker_id: String,
    /// Initial prompt or role description for the worker
    pub prompt: String,
    /// Current lifecycle status
    pub status: WorkerStatus,
    /// Unix timestamp (seconds) when the worker began its current task.
    /// Set when status transitions to Running; cleared otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_secs: Option<u64>,
}

impl PartialEq for WorkerHandle {
    fn eq(&self, other: &Self) -> bool {
        self.worker_id == other.worker_id
            && self.prompt == other.prompt
            && self.status == other.status
    }
}
impl Eq for WorkerHandle {}

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

// ── Supervisor / retry / summary ──

/// Retry strategy for failed or timed-out workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retry)
    pub max_retries: u32,
    /// Delay between retry attempts in seconds
    pub retry_delay_secs: u64,
    /// If true, reassign the task to a different worker after max retries
    pub reassign_on_exhausted: bool,
    /// Timeout in seconds for a single worker task execution (0 = no timeout)
    pub task_timeout_secs: u64,
    /// Poll interval for health checks in seconds
    pub health_check_interval_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_delay_secs: 2,
            reassign_on_exhausted: true,
            task_timeout_secs: 300,
            health_check_interval_secs: 5,
        }
    }
}

/// A summary report aggregating all worker outcomes for a swarm session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmSummaryReport {
    /// Total number of workers spawned
    pub total_workers: u32,
    /// Number of workers that completed successfully
    pub completed: u32,
    /// Number of workers that failed
    pub failed: u32,
    /// Number of workers that timed out
    pub timed_out: u32,
    /// Number of tasks that were reassigned
    pub tasks_reassigned: u32,
    /// Per-worker detail reports
    pub worker_reports: Vec<AgentReport>,
    /// Timestamp when the summary was generated
    pub generated_at_secs: u64,
}

impl SwarmSummaryReport {
    /// Build a summary from a collection of worker reports.
    pub fn from_reports(reports: &[AgentReport]) -> Self {
        let completed = reports.iter().filter(|r| r.status == WorkerStatus::Completed).count() as u32;
        let failed = reports.iter().filter(|r| r.status == WorkerStatus::Failed).count() as u32;
        let timed_out = reports.iter().filter(|r| r.status == WorkerStatus::TimedOut).count() as u32;
        Self {
            total_workers: reports.len() as u32,
            completed,
            failed,
            timed_out,
            tasks_reassigned: 0,
            worker_reports: reports.to_vec(),
            generated_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Check whether all workers have reached a terminal state.
    pub fn all_terminal(&self) -> bool {
        self.completed + self.failed + self.timed_out >= self.total_workers
    }

    /// Format a human-readable summary string.
    pub fn format(&self) -> String {
        format!(
            "Swarm Summary: {} workers total | {} completed | {} failed | {} timed out | {} reassigned",
            self.total_workers, self.completed, self.failed, self.timed_out, self.tasks_reassigned
        )
    }
}

/// A golden transcript entry for replay-based testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenTranscript {
    pub session_id: String,
    /// Serialized EventEnvelope payloads (JSON strings) for replay verification
    pub events: Vec<String>,
    /// Assertions to verify after replay
    pub verification_checks: Vec<TranscriptCheck>,
}

/// A single assertion in a golden transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptCheck {
    pub description: String,
    pub event_id: Option<String>,
    pub must_contain_text: Option<String>,
    pub must_have_tool_call: Option<String>,
    pub must_have_usage: bool,
}
