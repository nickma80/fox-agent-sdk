use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

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
