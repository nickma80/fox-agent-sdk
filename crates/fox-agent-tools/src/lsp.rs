use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const OPERATIONS: &[&str] = &[
    "goToDefinition",
    "findReferences",
    "hover",
    "documentSymbol",
    "workspaceSymbol",
    "goToImplementation",
    "prepareCallHierarchy",
    "incomingCalls",
    "outgoingCalls",
];

fn resolve_path(working_dir: Option<&Path>, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base) = working_dir {
        base.join(path)
    } else {
        path.to_path_buf()
    }
}

pub struct LspTool;

impl LspTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct LspInput {
    operation: String,
    file_path: String,
    line: u32,
    character: u32,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Run an LSP operation. Stub only: LSP is not integrated yet, so prefer grep/read for symbol inspection."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation", "file_path", "line", "character"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": OPERATIONS,
                    "description": "LSP operation."
                },
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line."
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based character."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: LspInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid lsp input: {e}"),
        })?;

        if !OPERATIONS.contains(&params.operation.as_str()) {
            return Err(ToolError::Message {
                message: format!("Unsupported LSP operation: {}", params.operation),
            });
        }

        let path = resolve_path(ctx.working_dir.as_deref(), Path::new(&params.file_path));
        if !path.exists() {
            return Err(ToolError::Message {
                message: format!("File not found: {}", params.file_path),
            });
        }

        Ok(ToolOutput {
            text: format!(
                "LSP is not integrated yet. Requested: {} at {}:{}:{}.\nUse grep or read to inspect symbols.",
                params.operation, params.file_path, params.line, params.character
            ),
            is_error: false,
            json: None,
        })
    }
}
