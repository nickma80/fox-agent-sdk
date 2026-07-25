use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The role of a message author in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    /// System instructions / prompt
    System,
    /// Human user message
    User,
    /// LLM assistant response
    Assistant,
    /// Tool execution result (injected into context)
    Tool,
}

/// A single message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Who sent this message
    pub role: Role,
    /// Content blocks (text, tool results, images, etc.)
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::Text { text: text.into() }] }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: vec![ContentBlock::Text { text: text.into() }] }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>, is_error: bool) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult { call_id: call_id.into(), text: text.into(), is_error }],
        }
    }

    /// Total character count across all content blocks, used for context
    /// pressure estimation.
    pub fn total_chars(&self) -> usize {
        self.content.iter().map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Reasoning { text } => text.len(),
            ContentBlock::ToolUse { name, input, .. } => {
                name.len() + serde_json::to_string(input).map(|s| s.len()).unwrap_or(0)
            }
            ContentBlock::Image { data, .. } => data.len(),
            ContentBlock::ToolResult { text, call_id, .. } => text.len() + call_id.len(),
            ContentBlock::NarrativeSummary { text } => text.len(),
        }).sum()
    }
}

/// A single content block within a message (text, reasoning, image, tool call, tool result, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Plain text content
    #[serde(rename = "text")]
    Text { text: String },
    /// Model reasoning/thinking content (not part of the final answer).
    /// Persisted so it can be sent back on subsequent tool-calling turns.
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
    /// Model tool-use request stored in an assistant message (for conversation history).
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
    /// Base64-encoded image sent by the user.
    #[serde(rename = "image")]
    Image { media_type: String, data: String },
    /// Tool execution result injected into the conversation
    #[serde(rename = "tool_result")]
    ToolResult { call_id: String, text: String, is_error: bool },
    /// L4 archival narrative summary — a compact, structured record of a turn
    /// range produced by the compaction summarizer. Accumulates over time
    /// rather than replacing earlier summaries.
    #[serde(rename = "narrative_summary")]
    NarrativeSummary { text: String },
}
