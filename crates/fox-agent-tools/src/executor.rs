use fox_agent_core::{SandboxError, SandboxOperation, Tool, ToolContext, ToolDefinition, ToolError, ToolOutput, WorkspaceSandbox};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde_json::Value;

use crate::agentgrep::AgentGrepTool;
use crate::bash::BashTool;
use crate::memory::MemoryTool;
use crate::edit::EditTool;
use crate::glob::GlobTool;
use crate::goals::GoalTool;
use crate::grep::GrepTool;
use crate::invalid::InvalidTool;
use crate::ls::LsTool;
use crate::lsp::LspTool;
use crate::plans::PlanTool;
use crate::read::ReadTool;
use crate::todos::TodoTool;
use crate::webfetch::WebFetchTool;
use crate::websearch::WebSearchTool;
use crate::write::WriteTool;

/// Mapping of tool names to their path input fields and expected operation types.
/// Used by the sandbox to validate file system access before tool execution.
const FILE_TOOLS: &[(&str, &str, SandboxOperation)] = &[
    ("read", "file_path", SandboxOperation::Read),
    ("write", "file_path", SandboxOperation::Write),
    ("edit", "file_path", SandboxOperation::Write),
    ("glob", "path", SandboxOperation::Read),
    ("grep", "path", SandboxOperation::Read),
    ("ls", "path", SandboxOperation::Read),
];

/// Thread-safe registry and executor for tools.
#[derive(Clone, Default)]
pub struct ToolExecutor {
    /// Map of tool name → tool implementation
    tools: Arc<RwLock<HashMap<String, Arc<dyn Tool>>>>,
    /// Optional workspace sandbox for path validation (shared for writable access)
    sandbox: Arc<RwLock<Option<Arc<WorkspaceSandbox>>>>,
}

impl ToolExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new ToolExecutor with a workspace sandbox.
    pub fn with_sandbox(sandbox: WorkspaceSandbox) -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
            sandbox: Arc::new(RwLock::new(Some(Arc::new(sandbox)))),
        }
    }

    /// Set or replace the workspace sandbox after construction.
    pub async fn set_sandbox(&self, sandbox: Option<WorkspaceSandbox>) {
        *self.sandbox.write().await = sandbox.map(Arc::new);
    }

    /// Register a tool. Overwrites previous registration for the same name.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.write().await.insert(name, tool);
    }

    /// Unregister a tool by name.
    pub async fn unregister_tool(&self, name: &str) {
        self.tools.write().await.remove(name);
    }

    /// Get ToolDefinitions for all registered tools.
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut defs: Vec<ToolDefinition> = self
            .tools
            .read()
            .await
            .values()
            .map(|t| t.to_definition())
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Look up and execute a tool by name.
    /// Before dispatching, validates the tool call against the workspace sandbox if configured.
    pub async fn execute_tool(
        &self,
        name: &str,
        input: Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        // Sandbox validation before dispatch
        if let Some(ref sandbox) = *self.sandbox.read().await {
            validate_tool_call(sandbox, name, &input, &ctx).map_err(|e| ToolError::Message {
                message: format!("sandbox: {e}"),
            })?;
        }

        let tool = {
            let map = self.tools.read().await;
            map.get(name).cloned()
        };
        let Some(tool) = tool else {
            return Err(ToolError::Message {
                message: format!("tool not found: {name}"),
            });
        };
        tool.execute(input, ctx).await
    }
}

/// Validate a tool call against the workspace sandbox.
///
/// For file-path-based tools (read, write, edit, glob, grep, ls),
/// extracts the path parameter from the input JSON, resolves it
/// against the working directory, and validates it.
///
/// For bash, validates the working directory for execute operations.
pub fn validate_tool_call(
    sandbox: &WorkspaceSandbox,
    tool_name: &str,
    input: &Value,
    ctx: &ToolContext,
) -> Result<(), SandboxError> {
    // Check file-path-based tools
    for &(name, param, op) in FILE_TOOLS {
        if tool_name == name {
            if let Some(path_str) = input.get(param).and_then(|v| v.as_str()) {
                let path = Path::new(path_str);
                let resolved = resolve_path(ctx.working_dir.as_deref(), path);
                sandbox.validate_path(&resolved, op)?;
            }
            return Ok(());
        }
    }

    // For bash, validate the working directory against sandbox root
    if tool_name == "bash" {
        if let Some(ref wd) = ctx.working_dir {
            if !sandbox.allow_exec_outside && !wd.starts_with(&sandbox.root_dir) {
                return Err(SandboxError::AccessDenied {
                    path: wd.clone(),
                    operation: SandboxOperation::Execute,
                    root: sandbox.root_dir.clone(),
                });
            }
        }
        return Ok(());
    }

    Ok(())
}

/// Resolve a path against an optional working directory.
///
/// If the path is absolute, returns it as-is.
/// If relative and a working directory is provided, joins them.
/// Otherwise returns the path as-is.
fn resolve_path(working_dir: Option<&Path>, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base) = working_dir {
        base.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Register all default built-in tools on an executor.
pub async fn register_default_tools(executor: &ToolExecutor) {
    executor.register_tool(Arc::new(ReadTool)).await;
    executor.register_tool(Arc::new(WriteTool)).await;
    executor.register_tool(Arc::new(EditTool)).await;
    executor.register_tool(Arc::new(GrepTool)).await;
    executor.register_tool(Arc::new(GlobTool)).await;
    executor.register_tool(Arc::new(LsTool)).await;
    executor.register_tool(Arc::new(BashTool)).await;
    executor.register_tool(Arc::new(WebFetchTool::new())).await;
    executor.register_tool(Arc::new(WebSearchTool::new())).await;
    executor.register_tool(Arc::new(TodoTool)).await;
    executor.register_tool(Arc::new(PlanTool)).await;
    executor.register_tool(Arc::new(GoalTool)).await;
    executor.register_tool(Arc::new(LspTool)).await;
    executor.register_tool(Arc::new(InvalidTool)).await;
    executor.register_tool(Arc::new(AgentGrepTool)).await;
    executor.register_tool(Arc::new(MemoryTool::with_manager(
        fox_agent_core::MemoryManager::new(&fox_agent_core::MemoryConfig::default()),
    ))).await;
}

/// Create a ToolExecutor pre-loaded with all default tools.
pub async fn default_tool_executor() -> ToolExecutor {
    let executor = ToolExecutor::new();
    register_default_tools(&executor).await;
    executor
}

#[cfg(test)]
mod tests {
    use super::*;
    use fox_agent_core::{ToolExecutionMode, WorkspaceSandbox};
    use std::path::PathBuf;

    fn make_context(working_dir: Option<PathBuf>) -> ToolContext {
        ToolContext {
            session_id: "test".into(),
            message_id: "m1".into(),
            tool_call_id: "tc1".into(),
            working_dir,
            execution_mode: ToolExecutionMode::Foreground,
            graceful_shutdown_requested: false,
        }
    }

    #[test]
    fn test_validate_tool_call_read_within_sandbox() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({"file_path": "src/main.rs"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "read", &input, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_tool_call_read_outside_sandbox() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({"file_path": "/etc/passwd"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "read", &input, &ctx);
        assert!(result.is_err());
        match result {
            Err(SandboxError::AccessDenied { operation, .. }) => {
                assert_eq!(operation, SandboxOperation::Read);
            }
            _ => panic!("expected AccessDenied"),
        }
    }

    #[test]
    fn test_validate_tool_call_read_with_allow_outside() {
        let sandbox = WorkspaceSandbox::new("/safe/project").with_read_outside(true);
        let input = serde_json::json!({"file_path": "/etc/passwd"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "read", &input, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_tool_call_write_denied_outside() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({"file_path": "/tmp/output.txt"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "write", &input, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_tool_call_write_allowed_outside() {
        let sandbox = WorkspaceSandbox::new("/safe/project").with_write_outside(true);
        let input = serde_json::json!({"file_path": "/tmp/output.txt"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "write", &input, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_tool_call_bash_outside_denied() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({"command": "ls"});
        let ctx = make_context(Some(PathBuf::from("/tmp")));
        let result = validate_tool_call(&sandbox, "bash", &input, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_tool_call_bash_within_sandbox() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({"command": "ls"});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        let result = validate_tool_call(&sandbox, "bash", &input, &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_tool_call_unknown_tool() {
        let sandbox = WorkspaceSandbox::new("/safe/project");
        let input = serde_json::json!({});
        let ctx = make_context(Some(PathBuf::from("/safe/project")));
        // Unknown tools are not subject to sandbox validation
        let result = validate_tool_call(&sandbox, "webfetch", &input, &ctx);
        assert!(result.is_ok());
    }
}
