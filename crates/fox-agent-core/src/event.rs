use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::compaction::CompactionEvent;
use crate::interrupt::InjectedInterrupt;
use crate::memory::MemoryStateEvent;
use crate::provider::{ProviderError, TokenUsage};
use crate::tool::{ToolError, ToolOutput, ToolResultRouting};

// ── Outcome ──

/// Outcome of an agent turn.
#[derive(Clone, Debug)]
pub enum TurnOutcome {
    /// Turn completed successfully with final text
    Completed { text: String },
    /// Turn was cancelled (graceful shutdown)
    Cancelled,
    /// Turn paused; user must decide on a permission request
    RequiresUserDecision { request: PermissionRequest },
    /// Turn failed with an error
    Failed { error: AgentError },
}

// ── Permission types ──

/// Risk level for tool permission assessment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Safe read-only operation
    Low,
    /// Read + limited write (e.g. edit, todo)
    Medium,
    /// Arbitrary write or shell invocation
    High,
    /// Network access or destructive operation
    Critical,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::Low => write!(f, "low"),
            RiskLevel::Medium => write!(f, "medium"),
            RiskLevel::High => write!(f, "high"),
            RiskLevel::Critical => write!(f, "critical"),
        }
    }
}

/// A request to ask the user for permission before executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRequest {
    /// Unique request id for correlation with the decision
    pub request_id: String,
    /// Name of the tool awaiting permission
    pub tool_name: String,
    /// Human-readable prompt to show the user
    pub prompt: String,
    /// Risk level of the requested operation
    pub risk_level: RiskLevel,
    /// Unix timestamp when the request expires (None = no expiry)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Policy source that triggered the permission check
    /// (e.g. "denylist", "allowlist", "default:confirm", "default:deny")
    pub policy_source: String,
    /// Short human-readable summary of what the tool will do
    pub tool_summary: String,
}

impl PermissionRequest {
    pub fn new(tool_name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            tool_name: tool_name.into(),
            prompt: prompt.into(),
            risk_level: RiskLevel::Medium,
            expires_at: None,
            policy_source: String::new(),
            tool_summary: String::new(),
        }
    }

    /// Build a request with risk level and policy source.
    pub fn with_risk(
        mut self,
        level: RiskLevel,
        policy_source: impl Into<String>,
        tool_summary: impl Into<String>,
    ) -> Self {
        self.risk_level = level;
        self.policy_source = policy_source.into();
        self.tool_summary = tool_summary.into();
        self
    }

    /// Set the expiry timestamp.
    pub fn with_expiry(mut self, expires_at_secs: u64) -> Self {
        self.expires_at = Some(expires_at_secs);
        self
    }
}

/// User's decision in response to a PermissionRequest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Allow the tool to execute
    Allow,
    /// Deny execution with a reason
    Deny { reason: String },
}

/// Outcome of a tool permission check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionResult {
    /// Tool may execute immediately
    Allow,
    /// Tool is blocked with a reason
    Deny { reason: String },
    /// User must decide; return the request to the application layer
    AskUser { request: PermissionRequest },
}

// ── Error types ──

/// Top-level agent error type.
#[derive(Debug, Error, Clone)]
pub enum AgentError {
    /// Provider-level error (API failure, timeout, etc.)
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    /// Tool execution error
    #[error("tool: {0}")]
    Tool(#[from] ToolError),
    /// User denied permission
    #[error("permission denied: {reason}")]
    PermissionDenied { reason: String },
    /// Internal agent error (e.g. inconsistent state)
    #[error("agent internal error: {message}")]
    Internal { message: String },
    /// Budget exceeded (token or cost)
    #[error("budget exceeded: {message}")]
    BudgetExceeded { message: String },
    /// MCP-related error
    #[error("mcp: {message}")]
    Mcp { message: String },
}

impl AgentError {
    /// Classify into a stable error kind for structured event reporting.
    pub fn kind(&self) -> ErrorKind {
        match self {
            AgentError::Provider(_) => ErrorKind::Provider,
            AgentError::Tool(_) => ErrorKind::Tool,
            AgentError::PermissionDenied { .. } => ErrorKind::Permission,
            AgentError::Internal { .. } => ErrorKind::Internal,
            AgentError::BudgetExceeded { .. } => ErrorKind::BudgetExceeded,
            AgentError::Mcp { .. } => ErrorKind::Mcp,
        }
    }

    /// Whether this error is likely transient and worth retrying.
    ///
    /// Delegates to [`ProviderError::is_retryable`] for provider errors.
    /// All other error kinds are considered permanent.
    pub fn is_retryable(&self) -> bool {
        match self {
            AgentError::Provider(e) => e.is_retryable(),
            _ => false,
        }
    }
}

/// Lightweight error kind label used in AgentEvent::Error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Provider,
    Tool,
    Permission,
    Internal,
    Cancelled,
    BudgetExceeded,
    Mcp,
}

// ── Turn summary ──

/// Deterministically extracted, human-readable summary of an agent turn.
///
/// Produced by the SDK at turn end (no LLM calls) and emitted as
/// `AgentEvent::TurnSummary` so the application layer (e.g. fox-code) can
/// render a "how was the goal accomplished" panel instead of a raw
/// tool-call histogram. All fields are best-effort extracts from the
/// turn's message history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnSummary {
    /// The turn this summary covers.
    pub turn_id: u64,
    /// The user's request that started this turn (truncated).
    pub user_intent: String,
    /// Files created or modified during this turn (write/edit tools), deduplicated.
    pub files_modified: Vec<String>,
    /// Files read during this turn (read/glob/grep/ls), deduplicated and capped.
    pub files_read: Vec<String>,
    /// Key actions taken (non-read tools), capped.
    pub actions: Vec<String>,
    /// Failed tool calls formatted as `tool: error preview`, capped.
    pub failures: Vec<String>,
    /// Preview of the final assistant response (truncated).
    pub response_preview: String,
    /// Total number of tool calls executed in this turn.
    pub tool_call_count: u32,
    /// Whether the turn ended as `TurnOutcome::Completed`.
    pub completed: bool,
    // ── Semantic fields ─────────────────────────────────────────────
    // LLM-generated, best-effort, only populated on the final turn of a
    // task (gated by the SDK's `final_turn_summary` toggle). When absent
    // the consumer renders the deterministic fields above only.
    /// How the goal was accomplished (one paragraph, grounded in the transcript).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accomplishment: Option<String>,
    /// Concrete changes made (e.g. "added shapefile write support in src/shp.rs").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    /// Caveats / things the user should watch out for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
    /// Known limitations of what was done (not covered / not tested / deferred).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_limitations: Vec<String>,
    /// Key decisions made and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
}

impl TurnSummary {
    /// A mostly-empty summary for turns with no user message yet.
    pub fn empty(turn_id: u64) -> Self {
        Self {
            turn_id,
            user_intent: String::new(),
            files_modified: Vec::new(),
            files_read: Vec::new(),
            actions: Vec::new(),
            failures: Vec::new(),
            response_preview: String::new(),
            tool_call_count: 0,
            completed: false,
            accomplishment: None,
            changes: Vec::new(),
            caveats: Vec::new(),
            known_limitations: Vec::new(),
            decisions: Vec::new(),
        }
    }
}

// ── Agent event types ──

/// Events emitted by the agent during turn execution.
/// The application layer subscribes via `tokio::sync::mpsc::Sender<AgentEvent>`.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// A turn loop iteration started
    TurnStart { turn_id: u64 },
    /// A turn loop iteration ended
    TurnEnd { turn_id: u64, outcome: TurnOutcome },
    /// Model started generating a message
    ModelMessageStart { message_id: String },
    /// A chunk of model-generated text arrived
    ModelTextDelta { text: String },
    /// A chunk of model reasoning/thinking arrived (DeepSeek reasoning_content,
    /// Anthropic extended thinking). Displayed separately from ModelTextDelta.
    ModelThinkingDelta { text: String },
    /// Model finished generating the current message
    ModelMessageEnd { message_id: String },
    /// Heartbeat emitted while the agent is still waiting for the next event
    /// from the model stream (e.g. a slow first byte or a stalled provider).
    /// Purely informational so the UI can show "still waiting" instead of
    /// appearing frozen; it does not affect turn control flow.
    WaitingForModel { elapsed_secs: u64 },
    /// Token usage statistics from the provider
    ModelUsage { usage: TokenUsage },
    /// A tool call has been initiated by the model
    ToolCallStart {
        call_id: String,
        name: String,
        input: Value,
    },
    /// A partial chunk of a tool call's input JSON, streamed while the model is
    /// still generating the arguments. Enables the UI to show progress like
    /// "generating edit repository.rs…" for large write/edit inputs. Purely
    /// informational; the executable call arrives as `ToolCallStart`.
    ToolInputDelta {
        index: usize,
        call_id: Option<String>,
        tool_name: Option<String>,
        delta: String,
    },
    /// A tool call completed (contains the tool output)
    ToolCallEnd { call_id: String, output: ToolOutput },
    /// Agent needs user permission before executing a tool
    PermissionRequest {
        request_id: String,
        tool_name: String,
        prompt: String,
        risk_level: String,
        policy_source: String,
        tool_summary: String,
    },
    /// A compaction event occurred
    Compaction { event: CompactionEvent },
    /// Memory injection state changed
    MemoryStateChanged { event: MemoryStateEvent },
    /// Memory was injected into the system prompt
    MemoryInjected { count: u32, memory_ids: Vec<String> },
    /// A soft interrupt was injected into the conversation
    SoftInterruptInjected { interrupt: InjectedInterrupt },
    /// A deterministic summary of the just-finished turn (goal, files touched,
    /// key actions, failures). Emitted immediately before the matching
    /// `TurnEnd`. The application layer renders it as the "turn summary".
    TurnSummary { summary: TurnSummary },
    /// An error occurred
    Error { error: AgentError },
    /// An MCP server connected successfully
    McpServerConnected { server_name: String },
    /// An MCP server disconnected
    McpServerDisconnected {
        server_name: String,
        error: Option<String>,
    },
    /// Tool is still executing (periodic heartbeat for progress UI).
    /// Emitted every 3s after the first 5s of tool execution. Purely
    /// informational; the TUI can use this to show elapsed time.
    ToolExecutionProgress {
        call_id: String,
        tool_name: String,
        elapsed_secs: u64,
    },
    /// A large tool result was externalized into the artifact store.
    ArtifactStored {
        artifact_id: String,
        tool_name: String,
        call_id: String,
        size_bytes: u64,
        artifact_type: String,
        retention_class: String,
        server_name: Option<String>,
        server_kind: Option<String>,
        transport: Option<String>,
        original_tool_name: Option<String>,
        externalized_reason: Option<String>,
    },
    /// An artifact was read back into the workflow via `artifact_read`.
    ArtifactRead {
        artifact_id: String,
        tool_name: String,
        returned_chars: usize,
        offset_chars: usize,
        limit_chars: usize,
        source_tool_name: Option<String>,
        artifact_type: Option<String>,
        server_name: Option<String>,
        server_kind: Option<String>,
        transport: Option<String>,
        original_tool_name: Option<String>,
    },
    /// Artifact store garbage collection reclaimed storage.
    ArtifactGc {
        scope: String,
        deleted: u64,
        kept: u64,
        bytes_freed: u64,
        session_quota_evictions: u64,
        store_quota_evictions: u64,
    },
    /// Plan progress updated. Emitted when the plan tool detects a change
    /// in completed/total ratios. Enables TUI to show overall progress.
    PlanProgress {
        completed: usize,
        total: usize,
        current_item: Option<String>,
    },
    /// A sub-agent task was dispatched (Phase 3).
    SubagentTaskStarted {
        task_id: String,
        objective: String,
        max_turns: u32,
    },
    /// A sub-agent task completed with a summary (Phase 3).
    SubagentTaskCompleted {
        task_id: String,
        outcome: String,
        findings_count: u32,
        evidence_count: u32,
        turns_used: u32,
        elapsed_secs: u64,
        summary_text: String,
    },
    /// Routing policy engine decided how to handle a tool result (Phase 4).
    RoutingDecision {
        tool_name: String,
        call_id: String,
        routing: ToolResultRouting,
        context_pressure: f64,
        output_size: usize,
        reason: Option<String>,
    },
}

/// Type alias for the sender side of the agent event channel.
pub type AgentEventTx = tokio::sync::mpsc::Sender<AgentEvent>;

// ── EventEnvelope ──

/// A timestamped, traced wrapper around [`AgentEvent`] for export and replay.
///
/// Every event emitted by the Agent is wrapped in an EventEnvelope before
/// being written to the event log. The envelope provides standard metadata
/// fields that enable ordered replay, distributed tracing, and audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    /// Unique event id (UUID v4)
    pub event_id: String,
    /// Owning session id
    pub session_id: String,
    /// Turn number within the session (1-based)
    pub turn_id: u64,
    /// Sequence number within the turn (monotonic, 0-based)
    pub seq: u64,
    /// Unix timestamp in seconds
    pub timestamp: u64,
    /// Optional trace id for distributed tracing correlation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Optional parent event id for causal linking
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    /// Source of the event (e.g. "agent", "provider", "tool", "user")
    pub source: String,
    /// The serialized inner event payload
    pub event: EnvelopePayload,
}

/// Serialized form of an AgentEvent for storage/export.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvelopePayload {
    TurnStart {
        turn_id: u64,
    },
    TurnEnd {
        turn_id: u64,
        outcome: String,
    },
    TurnSummary {
        summary: TurnSummary,
    },
    ModelMessageStart {
        message_id: String,
    },
    ModelTextDelta {
        text: String,
    },
    ModelThinkingDelta {
        text: String,
    },
    ModelMessageEnd {
        message_id: String,
    },
    WaitingForModel {
        elapsed_secs: u64,
    },
    ModelUsage {
        usage: TokenUsage,
    },
    ToolCallStart {
        call_id: String,
        name: String,
        input: Value,
    },
    ToolInputDelta {
        index: usize,
        call_id: Option<String>,
        tool_name: Option<String>,
        delta: String,
    },
    ToolCallEnd {
        call_id: String,
        output: ToolOutput,
    },
    PermissionRequest {
        request_id: String,
        tool_name: String,
        prompt: String,
        risk_level: String,
        policy_source: String,
        tool_summary: String,
    },
    Compaction {
        removed_messages: u64,
        kept_messages: u64,
        summary_chars: u64,
    },
    MemoryStateChanged {
        event: MemoryStateEvent,
    },
    MemoryInjected {
        count: u32,
        memory_ids: Vec<String>,
    },
    SoftInterruptInjected {
        content: String,
        urgent: bool,
    },
    McpServerConnected {
        server_name: String,
    },
    McpServerDisconnected {
        server_name: String,
        error: Option<String>,
    },
    ToolExecutionProgress {
        call_id: String,
        tool_name: String,
        elapsed_secs: u64,
    },
    ArtifactStored {
        artifact_id: String,
        tool_name: String,
        call_id: String,
        size_bytes: u64,
        artifact_type: String,
        retention_class: String,
        server_name: Option<String>,
        server_kind: Option<String>,
        transport: Option<String>,
        original_tool_name: Option<String>,
        externalized_reason: Option<String>,
    },
    ArtifactRead {
        artifact_id: String,
        tool_name: String,
        returned_chars: usize,
        offset_chars: usize,
        limit_chars: usize,
        source_tool_name: Option<String>,
        artifact_type: Option<String>,
        server_name: Option<String>,
        server_kind: Option<String>,
        transport: Option<String>,
        original_tool_name: Option<String>,
    },
    ArtifactGc {
        scope: String,
        deleted: u64,
        kept: u64,
        bytes_freed: u64,
        session_quota_evictions: u64,
        store_quota_evictions: u64,
    },
    PlanProgress {
        completed: usize,
        total: usize,
        current_item: Option<String>,
    },
    SubagentTaskStarted {
        task_id: String,
        objective: String,
        max_turns: u32,
    },
    SubagentTaskCompleted {
        task_id: String,
        outcome: String,
        findings_count: u32,
        evidence_count: u32,
        turns_used: u32,
        elapsed_secs: u64,
        summary_text: String,
    },
    RoutingDecision {
        tool_name: String,
        call_id: String,
        routing: ToolResultRouting,
        context_pressure: f64,
        output_size: usize,
        reason: Option<String>,
    },
    Error {
        kind: String,
        message: String,
    },
}

impl EventEnvelope {
    /// Create a new envelope with a generated event_id.
    pub fn new(
        session_id: impl Into<String>,
        turn_id: u64,
        seq: u64,
        source: impl Into<String>,
        event: EnvelopePayload,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            turn_id,
            seq,
            timestamp: crate::planning::now_secs(),
            trace_id: None,
            parent_event_id: None,
            source: source.into(),
            event,
        }
    }

    /// Set the trace_id for distributed tracing.
    pub fn with_trace_id(mut self, id: impl Into<String>) -> Self {
        self.trace_id = Some(id.into());
        self
    }

    /// Set the parent event id for causal linking.
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_event_id = Some(parent_id.into());
        self
    }

    /// Serialize this envelope as a single JSON line.
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl From<&AgentEvent> for EnvelopePayload {
    fn from(ev: &AgentEvent) -> Self {
        match ev {
            AgentEvent::TurnStart { turn_id } => EnvelopePayload::TurnStart { turn_id: *turn_id },
            AgentEvent::TurnEnd { turn_id, outcome } => EnvelopePayload::TurnEnd {
                turn_id: *turn_id,
                outcome: format!("{:?}", outcome),
            },
            AgentEvent::TurnSummary { summary } => EnvelopePayload::TurnSummary {
                summary: summary.clone(),
            },
            AgentEvent::ModelMessageStart { message_id } => EnvelopePayload::ModelMessageStart {
                message_id: message_id.clone(),
            },
            AgentEvent::ModelTextDelta { text } => {
                EnvelopePayload::ModelTextDelta { text: text.clone() }
            }
            AgentEvent::ModelThinkingDelta { text } => {
                EnvelopePayload::ModelThinkingDelta { text: text.clone() }
            }
            AgentEvent::ModelMessageEnd { message_id } => EnvelopePayload::ModelMessageEnd {
                message_id: message_id.clone(),
            },
            AgentEvent::WaitingForModel { elapsed_secs } => EnvelopePayload::WaitingForModel {
                elapsed_secs: *elapsed_secs,
            },
            AgentEvent::ModelUsage { usage } => EnvelopePayload::ModelUsage {
                usage: usage.clone(),
            },
            AgentEvent::ToolCallStart {
                call_id,
                name,
                input,
            } => EnvelopePayload::ToolCallStart {
                call_id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            AgentEvent::ToolInputDelta {
                index,
                call_id,
                tool_name,
                delta,
            } => EnvelopePayload::ToolInputDelta {
                index: *index,
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                delta: delta.clone(),
            },
            AgentEvent::ToolCallEnd { call_id, output } => EnvelopePayload::ToolCallEnd {
                call_id: call_id.clone(),
                output: output.clone(),
            },
            AgentEvent::PermissionRequest {
                request_id,
                tool_name,
                prompt,
                risk_level,
                policy_source,
                tool_summary,
            } => EnvelopePayload::PermissionRequest {
                request_id: request_id.clone(),
                tool_name: tool_name.clone(),
                prompt: prompt.clone(),
                risk_level: risk_level.clone(),
                policy_source: policy_source.clone(),
                tool_summary: tool_summary.clone(),
            },
            AgentEvent::Compaction { event } => EnvelopePayload::Compaction {
                removed_messages: event.removed_messages as u64,
                kept_messages: event.kept_messages as u64,
                summary_chars: event.summary_chars as u64,
            },
            AgentEvent::MemoryStateChanged { event } => EnvelopePayload::MemoryStateChanged {
                event: event.clone(),
            },
            AgentEvent::MemoryInjected { count, memory_ids } => EnvelopePayload::MemoryInjected {
                count: *count,
                memory_ids: memory_ids.clone(),
            },
            AgentEvent::SoftInterruptInjected { interrupt } => {
                EnvelopePayload::SoftInterruptInjected {
                    content: interrupt.content.clone(),
                    urgent: interrupt.urgent,
                }
            }
            AgentEvent::McpServerConnected { server_name } => EnvelopePayload::McpServerConnected {
                server_name: server_name.clone(),
            },
            AgentEvent::McpServerDisconnected { server_name, error } => {
                EnvelopePayload::McpServerDisconnected {
                    server_name: server_name.clone(),
                    error: error.clone(),
                }
            }
            AgentEvent::ToolExecutionProgress {
                call_id,
                tool_name,
                elapsed_secs,
            } => EnvelopePayload::ToolExecutionProgress {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                elapsed_secs: *elapsed_secs,
            },
            AgentEvent::ArtifactStored {
                artifact_id,
                tool_name,
                call_id,
                size_bytes,
                artifact_type,
                retention_class,
                server_name,
                server_kind,
                transport,
                original_tool_name,
                externalized_reason,
            } => EnvelopePayload::ArtifactStored {
                artifact_id: artifact_id.clone(),
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
                size_bytes: *size_bytes,
                artifact_type: artifact_type.clone(),
                retention_class: retention_class.clone(),
                server_name: server_name.clone(),
                server_kind: server_kind.clone(),
                transport: transport.clone(),
                original_tool_name: original_tool_name.clone(),
                externalized_reason: externalized_reason.clone(),
            },
            AgentEvent::ArtifactRead {
                artifact_id,
                tool_name,
                returned_chars,
                offset_chars,
                limit_chars,
                source_tool_name,
                artifact_type,
                server_name,
                server_kind,
                transport,
                original_tool_name,
            } => EnvelopePayload::ArtifactRead {
                artifact_id: artifact_id.clone(),
                tool_name: tool_name.clone(),
                returned_chars: *returned_chars,
                offset_chars: *offset_chars,
                limit_chars: *limit_chars,
                source_tool_name: source_tool_name.clone(),
                artifact_type: artifact_type.clone(),
                server_name: server_name.clone(),
                server_kind: server_kind.clone(),
                transport: transport.clone(),
                original_tool_name: original_tool_name.clone(),
            },
            AgentEvent::ArtifactGc {
                scope,
                deleted,
                kept,
                bytes_freed,
                session_quota_evictions,
                store_quota_evictions,
            } => EnvelopePayload::ArtifactGc {
                scope: scope.clone(),
                deleted: *deleted,
                kept: *kept,
                bytes_freed: *bytes_freed,
                session_quota_evictions: *session_quota_evictions,
                store_quota_evictions: *store_quota_evictions,
            },
            AgentEvent::PlanProgress {
                completed,
                total,
                current_item,
            } => EnvelopePayload::PlanProgress {
                completed: *completed,
                total: *total,
                current_item: current_item.clone(),
            },
            AgentEvent::SubagentTaskStarted {
                task_id,
                objective,
                max_turns,
            } => EnvelopePayload::SubagentTaskStarted {
                task_id: task_id.clone(),
                objective: objective.clone(),
                max_turns: *max_turns,
            },
            AgentEvent::SubagentTaskCompleted {
                task_id,
                outcome,
                findings_count,
                evidence_count,
                turns_used,
                elapsed_secs,
                summary_text,
            } => EnvelopePayload::SubagentTaskCompleted {
                task_id: task_id.clone(),
                outcome: outcome.clone(),
                findings_count: *findings_count,
                evidence_count: *evidence_count,
                turns_used: *turns_used,
                elapsed_secs: *elapsed_secs,
                summary_text: summary_text.clone(),
            },
            AgentEvent::RoutingDecision {
                tool_name,
                call_id,
                routing,
                context_pressure,
                output_size,
                reason,
            } => EnvelopePayload::RoutingDecision {
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
                routing: *routing,
                context_pressure: *context_pressure,
                output_size: *output_size,
                reason: reason.clone(),
            },
            AgentEvent::Error { error } => EnvelopePayload::Error {
                kind: format!("{:?}", error.kind()),
                message: error.to_string(),
            },
        }
    }
}

// ── Approval cache ──

/// The scope over which an approval decision is cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalScope {
    ThisTurn,
    ThisSession,
    ThisWorkspace,
}

/// A cached approval decision entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalCacheEntry {
    pub tool_name: String,
    pub workspace_key: Option<String>,
    pub decision: PermissionResult,
    pub scope: ApprovalScope,
    pub expires_at: Option<u64>,
    pub created_at: u64,
}

/// Audit trail record for a permission decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAuditEntry {
    pub timestamp: u64,
    pub session_id: String,
    pub turn_id: u64,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub decision: PermissionResult,
    pub request_id: String,
    pub latency_ms: u64,
}

/// Configuration for approval caching behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalCacheConfig {
    pub enabled: bool,
    /// Timeout in seconds before a cached approval expires. 0 = no expiry.
    pub ttl_secs: u64,
    /// Maximum cached entries per scope.
    pub max_entries: usize,
}

impl Default for ApprovalCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: 3600,
            max_entries: 100,
        }
    }
}
