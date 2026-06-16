use async_trait::async_trait;
use fox_agent_core::{Tool, ToolContext, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct InvalidTool;

impl InvalidTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct InvalidInput {
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[async_trait]
impl Tool for InvalidTool {
    fn name(&self) -> &str {
        "invalid"
    }

    fn description(&self) -> &str {
        "Reports that a tool call was invalid or the tool has been removed. Returned by the system when the model calls a tool that doesn't exist."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_name": {
                    "type": "string",
                    "description": "Name of the invalid tool that was called."
                },
                "reason": {
                    "type": "string",
                    "description": "Why the tool is invalid."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: InvalidInput = serde_json::from_value(input).map_err(|e| ToolError::Message {
            message: format!("invalid input: {e}"),
        })?;

        let tool_name = params.tool_name.unwrap_or_else(|| "unknown".to_string());
        let reason = params.reason.unwrap_or_else(|| "tool not found".to_string());

        let text = format!(
            "Invalid tool call: '{}'\n\nReason: {}\n\nCheck available tools and try again.",
            tool_name, reason
        );

        Ok(ToolOutput {
            text,
            is_error: true,
            json: Some(json!({
                "tool_name": tool_name,
                "reason": reason,
            })),
        })
    }
}
