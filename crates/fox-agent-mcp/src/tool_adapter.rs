//! Adapt MCP tool definitions into [`ToolDefinition`] for the agent tool system.

use crate::types::McpToolDef;

/// A tool definition that the agent's tool system understands.
/// Mirrors `fox_agent_core::ToolDefinition` without depending on it directly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    /// The input schema as a JSON value (kept as-is from MCP).
    pub parameters_schema: serde_json::Value,
}

/// Convert an MCP `McpToolDef` to a local `McpToolDefinition`.
pub fn mcp_tool_to_definition(
    server_name: &str,
    tool: &McpToolDef,
) -> McpToolDefinition {
    let name = format!("mcp://{}/{}", server_name, tool.name);
    let description = tool
        .description
        .clone()
        .unwrap_or_else(|| format!("MCP tool {}/{}", server_name, tool.name));
    McpToolDefinition {
        name,
        description,
        parameters_schema: tool.input_schema.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_name_prefixed_with_mcp_scheme() {
        let def = McpToolDef {
            name: "read_file".into(),
            description: Some("Read a file".into()),
            input_schema: json!({"type": "object", "properties": {}}),
        };
        let td = mcp_tool_to_definition("filesystem", &def);
        assert_eq!(td.name, "mcp://filesystem/read_file");
        assert_eq!(td.description, "Read a file");
    }

    #[test]
    fn tool_without_description_gets_auto_generated() {
        let def = McpToolDef {
            name: "query".into(),
            description: None,
            input_schema: json!({}),
        };
        let td = mcp_tool_to_definition("db", &def);
        assert!(td.description.contains("MCP tool db/query"));
    }
}
