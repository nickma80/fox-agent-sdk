use async_trait::async_trait;
use fox_agent_core::{
    Tool, ToolContext, ToolError, ToolOutput, intent_schema_property, truncate_to_chars,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::artifact_store::ArtifactStore;

const DEFAULT_LIMIT_CHARS: usize = 4000;
const MAX_LIMIT_CHARS: usize = 20_000;

pub struct ArtifactReadTool {
    store: Arc<dyn ArtifactStore>,
}

impl ArtifactReadTool {
    pub fn new(store: Arc<dyn ArtifactStore>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct ArtifactReadInput {
    artifact_id: String,
    #[serde(default)]
    offset_chars: Option<usize>,
    #[serde(default)]
    limit_chars: Option<usize>,
    #[serde(default)]
    intent: Option<String>,
}

#[async_trait]
impl Tool for ArtifactReadTool {
    fn name(&self) -> &str {
        "artifact_read"
    }

    fn description(&self) -> &str {
        "Read a previously externalized artifact by artifact_id. Use this only when a prior tool result references an artifact_id and you need the original content."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["artifact_id"],
            "properties": {
                "intent": intent_schema_property(),
                "artifact_id": {
                    "type": "string",
                    "description": "Artifact id returned by an earlier tool result."
                },
                "offset_chars": {
                    "type": "integer",
                    "description": "Character offset to start reading from. Default 0."
                },
                "limit_chars": {
                    "type": "integer",
                    "description": format!("Maximum number of characters to return. Default {DEFAULT_LIMIT_CHARS}, max {MAX_LIMIT_CHARS}.")
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let params: ArtifactReadInput =
            serde_json::from_value(input).map_err(|e| ToolError::Message {
                message: format!("invalid artifact_read input: {e}"),
            })?;
        let _ = &params.intent;
        let offset = params.offset_chars.unwrap_or(0);
        let limit = params
            .limit_chars
            .unwrap_or(DEFAULT_LIMIT_CHARS)
            .min(MAX_LIMIT_CHARS);

        let record = self
            .store
            .get_record(&params.artifact_id)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to load artifact metadata: {e}"),
            })?
            .ok_or_else(|| ToolError::Message {
                message: format!("artifact not found: {}", params.artifact_id),
            })?;

        let text = self
            .store
            .get_text(&params.artifact_id)
            .await
            .map_err(|e| ToolError::Message {
                message: format!("failed to load artifact payload: {e}"),
            })?
            .ok_or_else(|| ToolError::Message {
                message: format!("artifact payload not found: {}", params.artifact_id),
            })?;

        let total_chars = text.chars().count();
        if offset >= total_chars {
            return Ok(ToolOutput::new(format!(
                "Artifact `{}` has {} chars. Requested offset {} is past the end.",
                params.artifact_id, total_chars, offset
            )));
        }

        let sliced: String = text.chars().skip(offset).collect();
        let chunk = truncate_to_chars(&sliced, limit).to_string();
        let returned_chars = chunk.chars().count();
        let remaining = total_chars.saturating_sub(offset + returned_chars);

        Ok(ToolOutput {
            text: format!(
                "Artifact: {}\nType: {:?}\nClass: {:?}\nSize: {} bytes\nRange: chars {}..{} of {}\nRemaining chars after chunk: {}\n\n{}",
                record.artifact_id,
                record.artifact_type,
                record.class,
                record.size_bytes,
                offset,
                offset + returned_chars,
                total_chars,
                remaining,
                chunk
            ),
            is_error: false,
            json: Some(json!({
                "artifact_id": record.artifact_id,
                "offset_chars": offset,
                "limit_chars": limit,
                "returned_chars": returned_chars,
                "remaining_chars": remaining,
                "source_tool_name": record.metadata.get("tool_name").and_then(|v| v.as_str()),
                "artifact_type": format!("{:?}", record.artifact_type),
                "server_name": record.metadata.get("server_name").and_then(|v| v.as_str()),
                "server_kind": record.metadata.get("server_kind").and_then(|v| v.as_str()),
                "transport": record.metadata.get("transport").and_then(|v| v.as_str()),
                "original_tool_name": record.metadata.get("original_tool_name").and_then(|v| v.as_str()),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_store::FileArtifactStore;
    use fox_agent_core::{
        ArtifactProducer, ArtifactRetentionClass, ArtifactStoreConfig, ArtifactType,
        ToolExecutionMode,
    };

    #[tokio::test]
    async fn artifact_read_tool_reads_paginated_content() {
        let root = std::env::temp_dir().join(format!("fox-artifact-read-{}", uuid::Uuid::new_v4()));
        let mut cfg = ArtifactStoreConfig::default();
        cfg.enabled = true;
        cfg.max_artifact_bytes = 4096;
        cfg.gc_after_write = false;
        let store = Arc::new(FileArtifactStore::new(cfg, root.join("artifacts")));

        let record = store
            .put_text(
                "s1",
                ArtifactProducer::Tool {
                    tool_name: "read".to_string(),
                },
                ArtifactType::FileChunk,
                ArtifactRetentionClass::Ephemeral,
                "abcdefghij".to_string(),
                json!({}),
            )
            .await
            .unwrap()
            .record;

        let tool = ArtifactReadTool::new(store);
        let output = tool
            .execute(
                json!({
                    "artifact_id": record.artifact_id,
                    "offset_chars": 2,
                    "limit_chars": 4
                }),
                ToolContext {
                    session_id: "s1".to_string(),
                    message_id: "m1".to_string(),
                    tool_call_id: "tc1".to_string(),
                    working_dir: None,
                    execution_mode: ToolExecutionMode::Foreground,
                    graceful_shutdown_requested: false,
                    progress_tx: None,
                },
            )
            .await
            .unwrap();

        assert!(output.text.contains("Range: chars 2..6 of 10"));
        assert!(output.text.ends_with("cdef"));

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
