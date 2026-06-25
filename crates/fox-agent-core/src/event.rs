use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::compaction::CompactionEvent;
use crate::interrupt::InjectedInterrupt;
use crate::memory::MemoryStateEvent;
use crate::provider::{ProviderError, TokenUsage};
use crate::tool::{ToolError, ToolOutput};

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
    /// Token usage statistics from the provider
    ModelUsage { usage: TokenUsage },
    /// A tool call has been initiated by the model
    ToolCallStart { call_id: String, name: String, input: Value },
    /// A tool call completed (contains the tool output)
    ToolCallEnd { call_id: String, output: ToolOutput },
    /// Agent needs user permission before executing a tool
    PermissionRequest { request_id: String, tool_name: String, prompt: String, risk_level: String, policy_source: String, tool_summary: String },
    /// A compaction event occurred
    Compaction { event: CompactionEvent },
    /// Memory injection state changed
    MemoryStateChanged { event: MemoryStateEvent },
    /// Memory was injected into the system prompt
    MemoryInjected { count: u32, memory_ids: Vec<String> },
    /// A soft interrupt was injected into the conversation
    SoftInterruptInjected { interrupt: InjectedInterrupt },
    /// An error occurred
    Error { error: AgentError },
    /// An MCP server connected successfully
    McpServerConnected { server_name: String },
    /// An MCP server disconnected
    McpServerDisconnected { server_name: String, error: Option<String> },
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
    TurnStart { turn_id: u64 },
    TurnEnd { turn_id: u64, outcome: String },
    ModelMessageStart { message_id: String },
    ModelTextDelta { text: String },
    ModelThinkingDelta { text: String },
    ModelMessageEnd { message_id: String },
    ModelUsage { usage: TokenUsage },
    ToolCallStart { call_id: String, name: String, input: Value },
    ToolCallEnd { call_id: String, output: ToolOutput },
    PermissionRequest { request_id: String, tool_name: String, prompt: String, risk_level: String, policy_source: String, tool_summary: String },
    Compaction { removed_messages: u64, kept_messages: u64, summary_chars: u64 },
    MemoryStateChanged { event: MemoryStateEvent },
    MemoryInjected { count: u32, memory_ids: Vec<String> },
    SoftInterruptInjected { content: String, urgent: bool },
    McpServerConnected { server_name: String },
    McpServerDisconnected { server_name: String, error: Option<String> },
    Error { kind: String, message: String },
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
            AgentEvent::ModelMessageStart { message_id } => EnvelopePayload::ModelMessageStart {
                message_id: message_id.clone(),
            },
            AgentEvent::ModelTextDelta { text } => EnvelopePayload::ModelTextDelta {
                text: text.clone(),
            },
            AgentEvent::ModelThinkingDelta { text } => EnvelopePayload::ModelThinkingDelta {
                text: text.clone(),
            },
            AgentEvent::ModelMessageEnd { message_id } => EnvelopePayload::ModelMessageEnd {
                message_id: message_id.clone(),
            },
            AgentEvent::ModelUsage { usage } => EnvelopePayload::ModelUsage {
                usage: usage.clone(),
            },
            AgentEvent::ToolCallStart { call_id, name, input } => EnvelopePayload::ToolCallStart {
                call_id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            AgentEvent::ToolCallEnd { call_id, output } => EnvelopePayload::ToolCallEnd {
                call_id: call_id.clone(),
                output: output.clone(),
            },
            AgentEvent::PermissionRequest { request_id, tool_name, prompt, risk_level, policy_source, tool_summary } => {
                EnvelopePayload::PermissionRequest {
                    request_id: request_id.clone(),
                    tool_name: tool_name.clone(),
                    prompt: prompt.clone(),
                    risk_level: risk_level.clone(),
                    policy_source: policy_source.clone(),
                    tool_summary: tool_summary.clone(),
                }
            }
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
            AgentEvent::McpServerConnected { server_name } => {
                EnvelopePayload::McpServerConnected { server_name: server_name.clone() }
            }
            AgentEvent::McpServerDisconnected { server_name, error } => {
                EnvelopePayload::McpServerDisconnected {
                    server_name: server_name.clone(),
                    error: error.clone(),
                }
            }
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
        Self { enabled: true, ttl_secs: 3600, max_entries: 100 }
    }
}
