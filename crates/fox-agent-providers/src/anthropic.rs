use async_stream::try_stream;
use async_trait::async_trait;
use fox_agent_core::{
    EventStream, Message, Provider, ProviderError, StreamEvent, TokenUsage, ToolDefinition,
};
use futures::{stream, StreamExt};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use fox_agent_core::ProviderConfig;
use crate::util::build_headers;

// ── Provider ──

#[derive(Clone)]
pub struct AnthropicCompatibleProvider {
    client: Client,
    cfg: ProviderConfig,
}

impl AnthropicCompatibleProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|err| ProviderError::Message {
                message: format!("failed to build provider client: {err}"),
            })?;
        Ok(Self { client, cfg })
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.cfg.base_url.trim_end_matches('/'))
    }

    fn build_headers(&self) -> Result<HeaderMap, ProviderError> {
        build_headers(&self.cfg)
    }

    fn build_payload(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
    ) -> AnthropicMessagesRequest {
        let system = [system_static.trim(), system_dynamic.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let messages = messages
            .iter()
            .filter_map(AnthropicMessage::from_sdk_message)
            .collect::<Vec<_>>();

        let tools = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|tool| AnthropicTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.parameters_schema.clone(),
                    })
                    .collect(),
            )
        };

        AnthropicMessagesRequest {
            model: model_id.to_string(),
            system: if system.is_empty() { None } else { Some(system) },
            messages,
            tools,
            max_tokens: 4096,
            stream: self.cfg.use_streaming_api,
        }
    }

    fn response_to_events(
        &self,
        response: AnthropicMessagesResponse,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = Vec::new();
        for block in response.content {
            match block {
                AnthropicResponseContentBlock::Text { text } => {
                    if !text.is_empty() {
                        events.push(StreamEvent::TextDelta { text });
                    }
                }
                AnthropicResponseContentBlock::ToolUse { id, name, input } => {
                    events.push(StreamEvent::ToolUse { id, name, input });
                }
            }
        }
        if let Some(usage) = response.usage.map(TokenUsage::from) {
            events.push(StreamEvent::Usage { usage });
        }
        events.push(StreamEvent::MessageStop { stop_reason: None });
        Ok(events)
    }
}

#[async_trait]
impl Provider for AnthropicCompatibleProvider {
    async fn complete(
        &self,
        model_id: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream, ProviderError> {
        let payload = self.build_payload(model_id, messages, tools, system_static, system_dynamic);
        let response = self
            .client
            .post(self.endpoint())
            .headers(self.build_headers()?)
            .json(&payload)
            .send()
            .await
            .map_err(|err| ProviderError::Message {
                message: format!("provider request failed: {err}"),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Message {
                message: format!("provider returned {status}: {body}"),
            });
        }

        if self.cfg.use_streaming_api {
            return Ok(parse_anthropic_stream(response));
        }

        let response = response
            .json::<AnthropicMessagesResponse>()
            .await
            .map_err(|err| ProviderError::Message {
                message: format!("invalid provider response: {err}"),
            })?;

        Ok(stream::iter(self.response_to_events(response)?.into_iter().map(Ok)).boxed())
    }

    fn name(&self) -> &str {
        &self.cfg.provider_name
    }
}

// ── Request / Response types (Anthropic-specific) ──

/// Anthropic Messages API request.
#[derive(Debug, Clone, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    max_tokens: u32,
    stream: bool,
}

/// A single message in an Anthropic API request.
#[derive(Debug, Clone, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicRequestContentBlock>,
}

impl AnthropicMessage {
    fn from_sdk_message(message: &Message) -> Option<Self> {
        match message.role {
            fox_agent_core::Role::System => None,
            fox_agent_core::Role::User => Some(Self {
                role: "user".to_string(),
                content: vec![AnthropicRequestContentBlock::Text {
                    text: crate::util::extract_text_content(message),
                }],
            }),
            fox_agent_core::Role::Assistant => Some(Self {
                role: "assistant".to_string(),
                content: vec![AnthropicRequestContentBlock::Text {
                    text: crate::util::extract_text_content(message),
                }],
            }),
            fox_agent_core::Role::Tool => {
                let (tool_use_id, content) = crate::util::extract_tool_result(message);
                Some(Self {
                    role: "user".to_string(),
                    content: vec![AnthropicRequestContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    }],
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum AnthropicRequestContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: String },
}

/// Anthropic tool definition.
#[derive(Debug, Clone, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

/// Anthropic Messages API response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicResponseContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

impl From<AnthropicUsage> for TokenUsage {
    fn from(value: AnthropicUsage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: value.input_tokens + value.output_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum AnthropicResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
}

// ── Streaming (SSE) support ──

/// In-progress `tool_use` block being assembled from `input_json_delta` chunks.
#[derive(Debug, Clone, Default)]
struct StreamingToolBlock {
    id: String,
    name: String,
    partial_json: String,
}

/// Parse Anthropic's Messages streaming (SSE) response into `StreamEvent`s.
///
/// Handles the event sequence: `message_start` (input usage) →
/// `content_block_start` (text / thinking / tool_use) →
/// `content_block_delta` (`text_delta` / `thinking_delta` / `input_json_delta`)
/// → `content_block_stop` (emits `ToolUse` for a completed tool block) →
/// `message_delta` (output usage + stop_reason) → `message_stop`.
///
/// `input_json_delta` fragments are streamed as `ToolInputDelta` for progress
/// display; the fully parsed input is emitted as `ToolUse` at block stop.
fn parse_anthropic_stream(response: reqwest::Response) -> EventStream {
    let byte_stream = response.bytes_stream();

    Box::pin(try_stream! {
        let mut buffer = String::new();
        // index -> tool block being accumulated (only tool_use blocks tracked).
        let mut tool_blocks: std::collections::HashMap<usize, StreamingToolBlock> =
            std::collections::HashMap::new();
        let mut input_tokens: u32 = 0;
        let mut stop_reason: Option<String> = None;
        futures::pin_mut!(byte_stream);

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(|err| ProviderError::Message {
                message: format!("failed to read streaming response: {err}"),
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(idx) = buffer.find('\n') {
                let mut line = buffer.drain(..=idx).collect::<String>();
                while line.ends_with(['\n', '\r']) {
                    line.pop();
                }
                // Only interested in `data:` lines; the JSON carries a `type`.
                if !line.starts_with("data:") { continue; }
                let data = line["data:".len()..].trim();
                if data.is_empty() { continue; }

                let event: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match event_type {
                    "message_start" => {
                        if let Some(u) = event.pointer("/message/usage/input_tokens").and_then(|v| v.as_u64()) {
                            input_tokens = u as u32;
                        }
                    }
                    "content_block_start" => {
                        let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if let Some(block) = event.get("content_block") {
                            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                                tool_blocks.insert(index, StreamingToolBlock {
                                    id, name, partial_json: String::new(),
                                });
                            }
                        }
                    }
                    "content_block_delta" => {
                        let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if let Some(delta) = event.get("delta") {
                            match delta.get("type").and_then(|v| v.as_str()) {
                                Some("text_delta") => {
                                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                        if !text.is_empty() {
                                            yield StreamEvent::TextDelta { text: text.to_string() };
                                        }
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                        if !text.is_empty() {
                                            yield StreamEvent::ThinkingDelta { text: text.to_string() };
                                        }
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                        if let Some(block) = tool_blocks.get_mut(&index) {
                                            block.partial_json.push_str(partial);
                                            if !partial.is_empty() {
                                                yield StreamEvent::ToolInputDelta {
                                                    index,
                                                    id: Some(block.id.clone()),
                                                    name: Some(block.name.clone()),
                                                    delta: partial.to_string(),
                                                };
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "content_block_stop" => {
                        let index = event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        if let Some(block) = tool_blocks.remove(&index) {
                            let input = if block.partial_json.trim().is_empty() {
                                serde_json::json!({})
                            } else {
                                serde_json::from_str(&block.partial_json).unwrap_or(serde_json::json!({}))
                            };
                            yield StreamEvent::ToolUse { id: block.id, name: block.name, input };
                        }
                    }
                    "message_delta" => {
                        if let Some(reason) = event.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                            stop_reason = Some(reason.to_string());
                        }
                        if let Some(out) = event.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                            let output_tokens = out as u32;
                            yield StreamEvent::Usage {
                                usage: TokenUsage {
                                    input_tokens,
                                    output_tokens,
                                    total_tokens: input_tokens + output_tokens,
                                    cache_read_input_tokens: None,
                                    cache_creation_input_tokens: None,
                                },
                            };
                        }
                    }
                    "message_stop" => {
                        yield StreamEvent::MessageStop { stop_reason: stop_reason.clone() };
                        return;
                    }
                    _ => {}
                }
            }
        }

        yield StreamEvent::MessageStop { stop_reason };
    })
}
