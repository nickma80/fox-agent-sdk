use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Progress events emitted by tools during long-running execution.
///
/// Tools send these through `ToolContext::progress_tx` to give the
/// application layer real-time visibility into tool execution.
#[derive(Debug, Clone)]
pub enum ToolProgressEvent {
    /// A line of stdout/stderr output from a command (e.g. bash).
    StdoutLine { line: String, stream: OutputStream },
    /// Tool-defined progress update.
    Progress { message: String, current: u64, total: Option<u64> },
}

/// Which output stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Describes a tool that the model can call (serialized in API requests).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Unique tool name (must match the name() returned by the Tool impl)
    pub name: String,
    /// Human-readable description of what the tool does
    pub description: String,
    /// JSON Schema describing the tool's input parameters
    pub parameters_schema: Value,
}

/// The output produced by executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    /// Human-readable text result
    pub text: String,
    /// Whether this output represents an error
    pub is_error: bool,
    /// Optional structured JSON result (e.g. exit_code, file metadata)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
}

impl ToolOutput {
    /// Convenience constructor for simple text-only output.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false, json: None }
    }

    /// Constructor for error output.
    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true, json: None }
    }
}

/// How a tool result should be routed after execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ToolResultRouting {
    /// Write the output directly into the working message stream.
    #[default]
    Inline,
    /// Keep only a compact summary in the message stream.
    SummarizeOnly,
    /// Store the full output externally and inject only a reference.
    Externalize,
    /// Delegate the task to a subagent instead of handling inline.
    DelegateToSubagent,
}

/// Retention class used by the artifact store for GC prioritization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ArtifactRetentionClass {
    /// Temporary artifact with short TTL.
    #[default]
    Ephemeral,
    /// Artifact referenced by summary/evidence.
    Referenced,
    /// Explicitly retained artifact.
    Pinned,
}

/// Write decision produced by the routing layer for large outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArtifactWriteDecision {
    /// Drop the full payload entirely.
    Drop,
    /// Keep only summary/reference data.
    SummaryOnly,
    /// Persist the full output as an artifact.
    StoreFull,
    /// Reuse an existing artifact by id.
    ReuseExisting { artifact_id: String },
}

/// Origin of a persisted artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArtifactProducer {
    Tool { tool_name: String },
    Mcp { server_name: String, tool_name: String },
    Subagent { task_id: String },
}

/// High-level artifact content category.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArtifactType {
    ToolOutput,
    SearchResults,
    WebPage,
    FileChunk,
    McpPayload,
    McpReadOnlyPayload,
    McpFilesystemSnapshot,
    McpBrowserSnapshot,
    McpExternalApiPayload,
    McpShellTranscript,
    SubagentIntermediate,
    Other(String),
}

/// Metadata record describing an externalized tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactRecord {
    pub artifact_id: String,
    pub session_id: String,
    pub producer: ArtifactProducer,
    pub artifact_type: ArtifactType,
    pub size_bytes: u64,
    pub content_hash: String,
    pub class: ArtifactRetentionClass,
    pub ref_count: u32,
    pub last_access_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: Value,
    pub storage_path: std::path::PathBuf,
}

// ── Sub-agent types (Phase 3) ──

/// A task delegated to a sub-agent for isolated exploration.
///
/// The sub-agent runs in its own context with a forked model and session,
/// so large intermediate results never pollute the main agent's context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTask {
    /// Unique id for tracking this task across events and artifacts.
    pub task_id: String,
    /// What the sub-agent should accomplish (one sentence).
    pub objective: String,
    /// Background context passed from the main agent to orient the sub-agent.
    pub context: String,
    /// Tool names to make available (empty = all registered tools).
    pub tools: Vec<String>,
    /// Hard cap on conversation turns before auto-termination.
    pub max_turns: u32,
    /// Wall-clock timeout in seconds.
    pub timeout_secs: u64,
}

/// Outcome of a sub-agent task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubagentOutcome {
    /// Task finished successfully.
    Completed,
    /// Reached the turn limit before finishing.
    TurnLimitReached,
    /// Timed out before finishing.
    TimeoutReached,
    /// Terminated with an error.
    Error(String),
}

/// A reference to an artifact containing evidence from sub-agent work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Id of the artifact in the store.
    pub artifact_id: String,
    /// Human-readable label describing what this evidence contains.
    pub label: String,
    /// Short preview snippet (first 200 chars).
    pub snippet: String,
}

/// Structured summary that the sub-agent returns to the main agent.
///
/// The main agent only sees this summary (a few hundred tokens), not the
/// sub-agent's full conversation history or raw tool outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentSummary {
    /// Matches the task that was dispatched.
    pub task_id: String,
    /// The original objective of the sub-agent task.
    pub objective: String,
    pub outcome: SubagentOutcome,
    /// Key discoveries in priority order.
    pub findings: Vec<String>,
    /// References to artifacts containing detailed evidence.
    pub evidence_refs: Vec<EvidenceRef>,
    /// Actionable recommendations for the main agent.
    pub recommendations: Vec<String>,
    /// Things the sub-agent is uncertain about.
    pub uncertainties: Vec<String>,
    /// Suggested follow-up queries or tasks.
    pub next_queries: Vec<String>,
    /// Token usage if the provider reported it (serialised form).
    pub token_usage: Option<Value>,
    /// Number of conversation turns consumed.
    pub turns_used: u32,
    /// Wall-clock time consumed (seconds).
    pub elapsed_secs: u64,
}

impl SubagentSummary {
    /// Format a condensed one-paragraph summary for injection into the
    /// main agent's context (typically 100-500 tokens).
    pub fn format_for_main_context(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("[sub-agent {}] {}", self.task_id, match &self.outcome {
            SubagentOutcome::Completed => "completed".to_string(),
            SubagentOutcome::TurnLimitReached => "reached turn limit".to_string(),
            SubagentOutcome::TimeoutReached => "timed out".to_string(),
            SubagentOutcome::Error(e) => format!("error: {e}"),
        }));
        if !self.findings.is_empty() {
            lines.push("Findings:".to_string());
            for f in &self.findings {
                lines.push(format!("- {f}"));
            }
        }
        if !self.evidence_refs.is_empty() {
            lines.push(format!(
                "Evidence: {} artifact(s) available for review.",
                self.evidence_refs.len()
            ));
        }
        if !self.recommendations.is_empty() {
            lines.push("Recommendations:".to_string());
            for r in &self.recommendations {
                lines.push(format!("- {r}"));
            }
        }
        if !self.uncertainties.is_empty() {
            lines.push("Uncertainties:".to_string());
            for u in &self.uncertainties {
                lines.push(format!("- {u}"));
            }
        }
        lines.join("\n")
    }
}

// ── End sub-agent types ──

/// Whether a tool runs in the foreground (blocking) or background.
#[derive(Debug, Clone)]
pub enum ToolExecutionMode {
    /// Tool runs synchronously and blocks the agent loop until completion
    Foreground,
    /// Tool runs asynchronously; agent loop continues immediately
    Background,
}

/// Execution context passed to every tool invocation.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Current agent session id
    pub session_id: String,
    /// Unique id of the message containing the tool call
    pub message_id: String,
    /// Unique id of the tool call itself
    pub tool_call_id: String,
    /// Current working directory (sandbox root)
    pub working_dir: Option<std::path::PathBuf>,
    /// Foreground vs background execution mode
    pub execution_mode: ToolExecutionMode,
    /// Whether a graceful shutdown has been requested
    pub graceful_shutdown_requested: bool,
    /// Optional channel for tools to emit progress events during long-running
    /// execution (e.g. bash stdout lines). Tools should check `is_some()` before
    /// sending; the agent loop sets this when a progress channel is available.
    pub progress_tx: Option<tokio::sync::mpsc::Sender<ToolProgressEvent>>,
}

impl ToolContext {
    /// Resolve a path against the working directory.
    ///
    /// Absolute paths are returned unchanged.  Relative paths are joined to
    /// `working_dir` when it is `Some`, otherwise resolved under CWD.
    pub fn resolve_path(&self, path: &std::path::Path) -> std::path::PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(base) = &self.working_dir {
            base.join(path)
        } else {
            path.to_path_buf()
        }
    }
}

/// Trait that all executable tools must implement.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name.
    fn name(&self) -> &str;
    /// Human-readable description for the model.
    fn description(&self) -> &str;
    /// JSON Schema of the tool's input parameters.
    fn parameters_schema(&self) -> Value;
    /// Execute the tool with the given input and context.
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;

    /// Build a ToolDefinition from this tool's metadata.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters_schema: self.parameters_schema(),
        }
    }
}

/// Standard `intent` field to add to every tool's JSON schema.
///
/// Use in `parameters_schema()` like:
/// ```ignore
/// "intent": intent_schema_property(),
/// ```
pub fn intent_schema_property() -> Value {
    serde_json::json!({
        "type": "string",
        "description": "Describe WHY you are calling this tool (one sentence). This helps the system understand your reasoning."
    })
}

/// Errors returned by tool execution.
#[derive(Debug, Error, Clone)]
pub enum ToolError {
    #[error("tool error: {message}")]
    Message { message: String },
    #[error("tool timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
}
