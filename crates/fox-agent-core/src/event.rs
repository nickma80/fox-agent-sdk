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

/// A request to ask the user for permission before executing a tool.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// Unique request id for correlation with the decision
    pub request_id: String,
    /// Name of the tool awaiting permission
    pub tool_name: String,
    /// Human-readable prompt to show the user
    pub prompt: String,
}

impl PermissionRequest {
    pub fn new(tool_name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            tool_name: tool_name.into(),
            prompt: prompt.into(),
        }
    }
}

/// User's decision in response to a PermissionRequest.
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    /// Allow the tool to execute
    Allow,
    /// Deny execution with a reason
    Deny { reason: String },
}

/// Outcome of a tool permission check.
#[derive(Debug, Clone)]
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
}

impl AgentError {
    /// Classify into a stable error kind for structured event reporting.
    pub fn kind(&self) -> ErrorKind {
        match self {
            AgentError::Provider(_) => ErrorKind::Provider,
            AgentError::Tool(_) => ErrorKind::Tool,
            AgentError::PermissionDenied { .. } => ErrorKind::Permission,
            AgentError::Internal { .. } => ErrorKind::Internal,
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
    PermissionRequest { request_id: String, tool_name: String, prompt: String },
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
}

/// Type alias for the sender side of the agent event channel.
pub type AgentEventTx = tokio::sync::mpsc::Sender<AgentEvent>;
